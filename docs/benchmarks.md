# Benchmarks and fault fixtures

The S26 tools measure verified CPU transfers and prepare the P10 lifetime
matrix. They change no transfer policy. Results describe the recorded host and
artifact; they are not hardware qualification or a general performance claim.

## CPU sweeps

[benches/run.py](https://github.com/OrbitInfra/OrbiTE/blob/main/benches/run.py)
runs the public Rust API and a test-only C++ caller of frozen Mooncake v0.3.14.
The C++ runtime is confined to the reference executable. CPU memcpy, ordinary
TCP and io_uring have matched rows; HP-TCP and SHM have OrbiTE-only rows. HP-TCP
uses `127.0.0.1` and `127.0.0.2` as two loopback rails.

The default workload is READ and WRITE at 4 KiB, 64 KiB, 1 MiB and 64 MiB, plus
batches of 8 and 64 four-KiB blocks. Contiguous memory-target blocks exercise
physical-owner merging. Gapped blocks use a stride of twice the block size and
must remain separate owners. File batches preserve one owner per public request
for independent CQE outcomes, including contiguous blocks; the runner checks
that mapping too. There is one caller, two configured workers per engine,
three warmup batches, 100 samples and three independent process trials. Trial
order alternates the two implementations. Both disable engine failover,
metrics, progress workers and runtime queues, and enable merging. TENT's
transport-local TCP retry defaults are pinned separately: three retries,
100 ms initial delay and a 2,000 ms cap. OrbiTE has no corresponding inner
TCP retry loop. These are healthy-path measurements, not retry benchmarks.
The manifest records
these settings, actual process thread counts, compiler versions, affinity,
source and binary hashes, filesystems and every command. Internal worker
arrangements differ: these are matched caller/worker settings, not identical
engine implementations. TENT's common and TE RPC client pools are each pinned
to the same worker count through their two explicit environment variables.

Each iteration changes the source pattern and resets the destination. Every
payload byte, gap and guard is checked after **every** transfer, including
warmup; corruption aborts without a result. The runner also corrupts a guard
in each implementation and checks rejection. It rejects missing checks,
wrong byte counts and incomplete/nonpositive samples. There is no unchecked
mode.

Timing covers batch allocation, submission, the same polling schedule (128
hot polls, then exponential sleeps capped at 200 microseconds), and batch free.
OrbiTE additionally checks local drain. Setup, registration, reset and full
verification are outside the interval. CPU time uses the whole process clock,
including both engines' threads during that interval. Throughput divides
verified payload bytes by the sum of measured batch times. p50 and nearest-rank
p99 are empirical per-batch latency, not per-block latency; 100 samples give
limited tail resolution. The CPU memcpy row is only a memory-copy control,
not an engine submission. Raw samples are retained so results can be recomputed.

File offsets, lengths and buffer addresses are four-KiB aligned. The runner
requires actual ext4 and tmpfs mounts and checks OrbiTE's descriptor route.
Both file profiles leave TCP installed for host registration (TENT does not
include IOURING in its memory-registration transport list), but requests carry
an explicit io_uring hint. TENT attempts O_DIRECT on both; on tmpfs the flag may be accepted without a
direct path. Each iteration initializes the file through buffered I/O before
the measured operation. These are warmed, page-cache/coherence-sensitive
measurements, **not cold SSD throughput**. Completion does not include fsync
and makes no durability claim. Maximum payload per batch is 64 MiB; the
allocation and guard extent is bounded separately.

On a Linux runner with the native dependencies from the
[CPU oracle](cpu-oracle.md), also install `liburing-dev` **before** building
TENT. Enable io_uring in the kernel and choose writable ext4 and tmpfs paths:

```bash
python3 tests/reference/run.py --fetch-reference --output "$OUT/reference"
python3 benches/run.py --reference "$OUT/reference" --output "$OUT/sweep" \
  --ext4 "$EXT4_DIR" --tmpfs /dev/shm --workers 2 --warmup 3 \
  --samples 100 --trials 3
```

Both output directories must be fresh and outside the checkout. The
`Frozen TENT CPU comparison` CI workflow runs this on stable and Rust 1.85 and
uploads `cpu-bench-*` artifacts. GPU, RDMA, GDS and EFA are emitted as
**not run (hardware)**, without estimated numbers. The scoped summary lives in
[qualification](qualification.md#cpu-benchmark-evidence).

## TCP profile and connection reuse

The pre-change profile is [run 42439141061](https://github.com/OrbitInfra/OrbiTE/actions/runs/42439141061),
`cpu-oracle (stable)`, revision `ec103db` (production sources unchanged from
`f0556448`). Its host `runnervm7aw5e` exposes four AMD EPYC 7763 logical CPUs,
Ubuntu 24.04, Linux 7.0.0-1017-azure and Rust 1.99.1. The reference is
`e1ea5d31bb5e46ff2df9f90e7c2fbaf52fcdee8c`. Both toolchains passed all sixteen
profile cases with byte checks. These are diagnostic observations, not timings
used in the comparison table.

`benches/run.py --profile-only` uses the same executables, sizes, layouts,
workers and warmup as S26. `strace -f -c -w` counts 100 samples minus a one-sample
setup control, divided by 99. `perf record` samples user CPU at a 10-microsecond
period with DWARF stacks and 1,000 samples per workload: millisecond sampling
misses short-lived batch threads. `perf stat` separately records process CPU,
context switches and migrations. Raw traces, call stacks and exact commands
remain in the workflow's `cpu-bench-*` artifacts, outside Git. Profiling includes
verification/setup; latency measurements below exclude them.

| Baseline calls per batch (READ / WRITE) | OrbiTE | TENT | Interpretation |
|---|---:|---:|---|
| TCP connect, single 4 KiB | 1 / 1 | 0 / 0 | TENT reuses warmed connections |
| TCP connect, gapped 64 × 4 KiB | 64 / 64 | 0 / 0 | One handshake per physical request is confirmed |
| TCP nanosleep, gapped 64 × 4 KiB | 219 / 236 | 129 / 128 | Traced polling sleeps include the common benchmark poll loop |
| TCP futex, gapped 64 × 4 KiB | 429 / 431 | 481 / 469 | This is not a count of locks or thread handoffs |
| Batch thread creation, every OrbiTE target | 1 / 1 | 0 / 0 | One caller→new batch-worker handoff, retained for a later PR |
| tmpfs ring setup / enter, single 4 KiB | 1 / 1 | 1 / 1 | No extra ring is created per request |
| tmpfs futex, single 4 KiB | 13.7 / 13.0 | approximately 0 / 0 | Publication, progress and worker retirement remain measurable |
| ext4 ring enter, gapped 64 × 4 KiB | 1.14 / 1.11 | 1 / 1 | Reaping can need another enter |
| ext4 statx, gapped 64 × 4 KiB | 65 / 65 | 0 / 0 | Per-request file/descriptor checks remain; no safety checks removed |

The single TCP READ's user samples attribute 18.2% to `recv`, 14.4% to
`sched_yield`, 12.1% to the main VDSO clock frame and 10.2% to
`TcpTransport::transfer`. For gapped READ/WRITE, `recv` is 9.9%/10.2%,
`sched_yield` 9.5%/9.2%, and `ControlChannel::poll_read` 2.1%/2.3%.
Pattern generation/verification accounts for another 17% and is outside the
benchmark interval. This supports removing direct TCP's hot-poll/backoff loop
as well as reconnects. No admission or loan frame reaches 1% of these samples;
inlining and sample size prevent treating that as proof of zero bookkeeping
cost. Those owners and checks stay unchanged.

The tmpfs READ profile instead attributes 23.0% to `Batch::status`, 4.6% to
`ProgressState::status`, 19.4% to the VDSO clock frame and 2.0% to batch
publication. The file path still creates a worker and publishes completion to
the polling caller. Ext4 READ samples include 1.2% `statx`, 0.7% selector and
0.5% preparation; pattern generation alone is 46.1%. Aligned ext4 route checks
pass, so these runs do not implicate an accidental buffered fallback. They do
not isolate the historical p99 tail's cause; storage and VM scheduling need
a separate pass.

Waiting and working cannot be inferred by subtracting multi-thread process CPU
from wall latency. In the separate `perf stat` run, mean wall / process CPU per
batch was 285 / 398 microseconds for one TCP READ, 8,861 / 16,408 for gapped TCP
READ, 174 / 176 for tmpfs READ, and 1,960 / 1,053 for gapped ext4 READ.
Under `strace`, summed sleep / futex / epoll wall time for gapped TCP READ was
63.9 / 249.2 / 123.4 ms per batch, versus 3.0 / 46.4 / 21.3 ms for gapped ext4
READ (plus 0.39 ms in `io_uring_enter`). These include concurrent idle threads
and tracer perturbation, **not critical-path percentages**. The CPU stack
samples distinguish hot polling from sleeping; the untraced paired sweeps
quantify the effect on actual batch latency and CPU cost.

The same `perf stat` processes report 23,674 / 30,312 context switches for
OrbiTE's gapped TCP READ / WRITE, versus TENT's 46,849 / 46,749. Each process
includes setup, three warmups and 100 samples. That is approximately
230 / 294 versus 455 / 454 switches per executed batch, including setup;
it is not an exact count of owner handoffs. The one new thread per OrbiTE
batch is the measurable caller-to-worker handoff. A syscall count alone
cannot assign every remaining scheduler switch to an owner.

The change follows frozen HP-TCP's clean/dirty lane ownership without adding a
worker: one exclusive epoch-scoped stream, deadline-bounded socket waits, and
source readiness monitoring in the existing control workers. Dirty streams
are retired, never replayed; admission, backing loans, per-request snapshots,
physical drain and sticky cleanup remain. The native frame bytes and offsets
are unchanged; a stream now carries successive complete TCP frames. See
[the owner contract](cpu-tcp.md) and its negative controls.

### Paired results and regressions

[Run 42441063520](https://github.com/OrbitInfra/OrbiTE/actions/runs/42441063520)
completed 480 checked rows **before and after on each toolchain**, comparing
`f0556448` with `de9fd60b`. Later commits change fixtures and reporting only.
Both sides of each pair share one GitHub `ubuntu-24.04` VM, compiler,
filesystems and parameters above. Stable (Rust 1.99.1) uses four EPYC 7763
logical CPUs, matching S26's processor class; Rust 1.85 uses four EPYC 9V45
logical CPUs. Both report hostname `runnervm7aw5e` and Linux 7.0.0-1017-azure;
that hostname alone does not identify a processor. Do not pool the toolchains'
absolute times. The order is before then after, with implementation order
alternating across the three trials, not randomized across revisions.

The stable results below are medians of the three trial statistics. The
[PR's complete tables](https://github.com/OrbitInfra/OrbiTE/pull/180) include
**every** workload/implementation case, throughput and every numerical
regression; all 480 raw trial rows per side remain in `cpu-bench-*` artifacts.
Reproduce those tables with `python3 benches/compare.py BEFORE AFTER`. It
rejects mismatched manifests, workload hashes, missing trials and unchecked data.

| Workload | OrbiTE p50 µs before → after | TENT p50 µs after | OrbiTE p99 µs before → after | OrbiTE CPU µs before → after |
|---|---:|---:|---:|---:|
| TCP, 1 × 4 KiB READ | 275.3 → 153.5 | 284.2 | 320.4 → 261.8 | 370.3 → 190.3 |
| TCP, 1 × 4 KiB WRITE | 277.0 → 160.6 | 281.6 | 339.3 → 288.2 | 385.2 → 215.2 |
| TCP, 64 × 4 KiB gapped READ | 8,591.0 → 4,231.4 | 6,175.9 | 9,133.9 → 4,556.8 | 15,908.6 → 5,449.1 |
| TCP, 64 × 4 KiB gapped WRITE | 9,331.6 → 6,296.2 | 6,071.6 | 9,856.6 → 6,641.5 | 16,946.6 → 7,055.2 |
| tmpfs, 1 × 4 KiB READ | 156.8 → 155.4 | 82.4 | 329.3 → 309.8 | 168.0 → 164.8 |
| tmpfs, 1 × 4 KiB WRITE | 194.4 → 194.2 | 82.6 | 326.5 → 305.1 | 247.4 → 243.9 |
| ext4, 64 × 4 KiB gapped READ | 926.5 → 933.2 | 1,018.8 | 16,517.1 → 17,418.5 | 1,107.6 → 1,041.9 |
| ext4, 64 × 4 KiB gapped WRITE | 873.3 → 892.3 | 917.5 | 7,269.4 → 8,969.2 | 1,096.4 → 1,116.3 |

The after profile confirms zero warmed TCP connects and zero `sched_yield`
calls per batch, versus 64 connects and 308/342 yields for scattered READ/WRITE.
Futex calls fall from 429/431 to 226/242; epoll waits rise from 67/67 to 75/77.
One batch-thread creation remains. Pattern generation now occupies 49%/48% of
the scattered user-CPU stacks, outside the measured interval; the very short
single-request after profiles have only 61/26 stack samples, so their individual
frame percentages are not a reliable ranking. The untraced CPU times above
are the evidence for the cost reduction.

The EPYC 9V45 pair independently reduces scattered READ/WRITE p50 by 46%/29%
and CPU by 64%/58%. Its resulting p50s are 2.26/3.50 ms versus TENT's
2.92/2.91 ms: WRITE remains 20% slower there. Regressions are not omitted:

- Stable TCP's 64 MiB WRITE p50 rises 0.3% (overlapping trial ranges), while
  its p99 and CPU improve. On 1.85, 64 MiB READ p50 rises 2.8%, p99 10.7%
  and throughput falls 3.2%, while CPU falls 4.2%. Blocking wakeups can trade
  latency for CPU on a large transfer whose handshake is already amortized;
  the contribution is not isolated, and the latency trial ranges overlap.
  This is not a claim of a large-transfer improvement.
- Untuned paths also vary. Stable HP-TCP 1 MiB READ p50 rises 17.3%, with
  overlapping ranges for all four statistics. Stable ext4 1 MiB READ p50
  rises 40.8% (nonoverlapping median ranges); unchanged TENT rises 23.1%
  on that case. Ext4 64-block contiguous WRITE p99 rises 51.7%, with overlapping
  trial ranges. On 1.85, ext4 eight-block gapped READ p99 rises 55.8%.
  Warmed page-cache/coherence and VM scheduling are plausible contributors;
  shared control-worker effects are not ruled out. The cause remains open.
- Every smaller regression is flagged in the complete tables. Memcpy and
  TENT controls do not execute the changed TCP pool; their changes show run,
  cache and scheduling variation. OrbiTE HP-TCP, SHM and file payload owners
  are unchanged, but their shared idle control worker changed. Range overlap
  is descriptive, not a significance test or permission to discard a result.

The standard [480-row run on both toolchains](https://github.com/OrbitInfra/OrbiTE/actions/runs/42441786027)
also passed, alongside [all feature gates and 50-repeat controls](https://github.com/OrbitInfra/OrbiTE/actions/runs/42441786080)
and the [installed-wheel matrix](https://github.com/OrbitInfra/OrbiTE/actions/runs/42441786046).
These are single-caller cloud-loopback and warmed-file results. No concurrent-caller,
physical two-host, GPU, RNIC or SSD performance claim follows. The small-file
pass below measures storage latency; ext4 tails and remaining bookkeeping cost
stay separate work.

## Small io_uring submission profile

[Run 42495924134](https://github.com/OrbitInfra/OrbiTE/actions/runs/42495924134)
profiles `4b831f3`, whose production sources are identical to `edec7937`.
The stable job uses four EPYC 9V74 logical CPUs and Rust 1.99.1; the Rust 1.85
job uses four EPYC 7763 logical CPUs. Both use Ubuntu 24.04, Linux
7.0.0-1017-azure, the frozen reference above and the same S26 parameters.
The eighteen byte-checked profiles add the HP-TCP 1 MiB READ/WRITE watch item
to the earlier sixteen cases. Raw profiles remain in `cpu-bench-*` artifacts.

The table uses the one-sample setup subtraction described above. Futex calls
are synchronization syscalls, not a count of ownership handoffs.

| Per 4 KiB tmpfs batch (READ / WRITE) | OrbiTE stable | OrbiTE 1.85 | TENT 1.85 |
|---|---:|---:|---:|
| User-thread creation (`clone`/`clone3`) | 1 / 1 | 1 / 1 | 0 / 0 |
| Ring setup | 1 / 1 | 1 / 1 | 1 / 1 |
| Ring enter | 1 / 1 | 1 / 1.01 | 1 / 1 |
| Futex | 11.26 / 11.85 | 13.21 / 13.08 | 0.01 / 0 |
| Polling sleeps | 6.65 / 6.65 | 5.41 / 5.45 | 0 / 0 |

This confirms one caller-to-new-worker handoff per OrbiTE batch, versus TENT's
caller submission. It rejects a repeated extra probe: four executed batches
(one sample plus three warmups) create five OrbiTE rings; 103 batches create
104. The one lazy probe is outside the measured interval. Both keep one ring
per batch. The aligned workload allocates **zero bounce buffers**, also checked
by `aligned_small_file_requests_need_no_bounce_allocation`; removing an
allocation would not address this target. Unaligned requests keep their
charged bounce owner.

On 1.85, the tmpfs READ/WRITE user stacks attribute 17.4%/17.0% to
`Batch::status`, 7.4%/6.0% to `ProgressState::status`, and 27.9%/32.1% to the
main VDSO clock frame. These are 258/265 samples, so small frame differences
are not precise rankings. The separate `perf stat` run measures mean
wall/process-CPU per batch of 174/180 µs READ and 240/284 µs WRITE, versus
TENT's 98/107 and 106/118 µs. Process context switches, including setup and
warmup, are 139/545 versus 211/234. Under tracing, OrbiTE's sleep/futex/epoll
wall sums are 0.72/0.47/12.43 ms for READ and 0.73/0.50/12.64 ms for WRITE.
Those include concurrent idle workers and tracer overhead; they are not
critical-path percentages. The status stacks and thread creation identify
handoff and polling work; only untraced matched sweeps measure the improvement.

The bounded change submits one contiguous host request of at most 64 KiB on
an idle batch's caller and makes one nonblocking CQ observation. A ready CQE
finishes through the normal prefix and admission owners. Otherwise the same
ring moves to the existing batch worker for drain. Runtime queuing, background
progress, eligible fallback, CUDA/pinned producer waits, vectors and larger
groups keep their existing worker path. No ring pool or coordinator is added.
A worker-start failure after submission quarantines ring, fd, loans and charges.
[The owner contract and controls](storage.md) preserve preflight, partial-submit
drain, short-prefix copyback and the destruction deadline.

### Small-file paired results

[Run 42499905741](https://github.com/OrbitInfra/OrbiTE/actions/runs/42499905741)
completed **480 checked rows before and after on each toolchain**, comparing
`edec7937` with `a52e7b7`. Both jobs use four EPYC 7763 logical CPUs, matching
S26's processor class, with Rust 1.99.1 or 1.85.0. Each revision pair shares
one VM, compiler, ext4/tmpfs mounts, workload hashes and the parameters above.
Revision order is before then after; implementation order alternates across
three trials. The complete [stable](https://github.com/OrbitInfra/OrbiTE/pull/183#issuecomment-6725185335)
and [1.85](https://github.com/OrbitInfra/OrbiTE/pull/183#issuecomment-6725185819)
tables include all 160 workload/implementation cases per toolchain, not just
improved cases. Each cell aggregates three checked trial rows. Reproduce them with
`python3 benches/compare.py BEFORE AFTER --changed-profile io_uring-ext4 --changed-profile io_uring-tmpfs`;
raw samples and profiles stay in the run's `cpu-bench-*` artifacts.

Stable medians of the three trial statistics:

| Workload | OrbiTE p50 µs before → after | TENT p50 µs after | OrbiTE p99 µs before → after | OrbiTE CPU µs before → after |
|---|---:|---:|---:|---:|
| tmpfs, 1 × 4 KiB READ | 156.0 → 51.0 | 83.7 | 293.6 → 84.2 | 166.3 → 54.9 |
| tmpfs, 1 × 4 KiB WRITE | 196.9 → 193.0 | 83.8 | 294.2 → 289.6 | 249.5 → 178.4 |
| ext4, 1 × 4 KiB READ | 313.6 → 313.1 | 227.9 | 631.3 → 425.7 | 264.0 → 249.3 |
| ext4, 1 × 4 KiB WRITE | 306.4 → 315.5 | 205.5 | 460.0 → 574.7 | 294.2 → 259.3 |
| ext4, 1 × 64 KiB READ | 407.5 → 402.2 | 357.6 | 611.2 → 559.7 | 291.7 → 251.4 |
| ext4, 1 × 64 KiB WRITE | 322.7 → 332.4 | 240.7 | 512.1 → 483.7 | 312.8 → 257.7 |
| ext4, 64 × 4 KiB gapped READ | 932.6 → 906.3 | 982.5 | 16,149.5 → 14,948.5 | 1,131.8 → 986.9 |
| ext4, 64 × 4 KiB gapped WRITE | 937.9 → 889.8 | 968.8 | 6,478.3 → 5,932.1 | 1,133.4 → 1,078.7 |
| HP-TCP, 1 × 1 MiB READ | 631.4 → 608.6 | — | 800.2 → 865.5 | 963.6 → 925.7 |

Small tmpfs READ p50 and CPU fall 67%. WRITE CPU falls 29%, but its p50
barely changes and remains **2.30× TENT**: pending I/O still needs the existing
worker. Rust 1.85 independently records READ p50 155.4→53.1 µs and CPU
162.3→58.2 µs; WRITE p50 is 195.6→194.2 µs and CPU 248.5→174.7 µs.
This is a ready-completion optimization, not removal of the asynchronous owner.

The after `strace` samples report zero thread creations and polling sleeps,
one setup/enter and about six futex calls per small tmpfs operation. Tracing
changes the CQE arrival race: it even makes WRITE ready on the caller. That
is **not** evidence that ordinary WRITEs avoid the worker. The less intrusive
1.85 CPU profile still puts 6.7% in `pthread_create` and 5.6% in
`Batch::start_worker` for WRITE; `Batch::status` falls to 1.6%. READ instead
spends 26.5%/12.8% in verification/pattern generation, outside the timed interval.
The separate `perf stat` after samples observe 23/250 context switches for
READ/WRITE, including setup and warmup, versus 139/545 in the before profile.
Only the untraced same-VM sweeps above support the performance comparison.

Regressions remain visible, including in untouched payload paths:

- Rust 1.85 ext4 64 KiB READ/WRITE p50 rises 15.0%/17.1%, with nonoverlapping
  trial ranges; CPU falls 15.6%/19.7%. Moving submission before the worker can
  change completion/wakeup timing when the CQE is not ready. Its contribution
  is not isolated: unchanged TENT READ p50 also rises 22.2%, while its WRITE
  p50 falls 3.0%. Stable's corresponding WRITE p50 rises 3.0%. There is no
  claim of a general direct-I/O latency improvement.
- Stable ext4 eight-block gapped READ p99 rises 92.4%, and 64 MiB WRITE p99
  rises 49.8%, with nonoverlapping ranges. These larger groups keep the worker
  path; warmed file/cache and VM effects remain unresolved. Gapped 64-block
  p99 is still 14.9/5.9 ms versus TENT's 2.2/2.6 ms. Tail diagnosis is separate.
- Stable's unchanged memcpy WRITE control rises 9.9% in p50, and TENT's rises
  7.1%; those controls do not execute this file owner. Rust 1.85 single TCP
  WRITE p50 rises 49.3% with overlapping ranges. HP-TCP and SHM payloads also
  remain unchanged. The tables flag **every** numerical regression: 130/160
  cases on stable and 119/160 on 1.85 have at least one worse statistic;
  95/86 of those have overlapping ranges for all four statistics. This shows
  substantial run variation, not proof that every regression is noise. Shared
  batch code generation and scheduling contributions are not isolated.

### HP-TCP watch item recheck

[Run 42501030618](https://github.com/OrbitInfra/OrbiTE/actions/runs/42501030618)
separately repeats all 480 checked rows before and after the TCP-pool change,
`f0556448` → `edec7937`, with the same S26 parameters. Each pair shares one VM;
stable uses four EPYC 9V74 logical CPUs and 1.85 uses four EPYC 9V45 logical
CPUs. These are not the EPYC 7763 small-file pair above.

| HP-TCP 1 MiB READ | p50 µs before → after | p99 µs before → after | CPU µs before → after |
|---|---:|---:|---:|
| Stable | 405.8 → 458.3 (+12.9%) | 596.2 → 581.9 | 647.7 → 648.9 |
| Rust 1.85 | 352.2 → 280.7 (−20.3%) | 487.9 → 360.0 | 493.3 → 434.8 |

All four statistics, including throughput, have overlapping trial ranges in
both pairs. Stable repeats the direction of the original +17.3% watch item,
but its CPU cost is flat and p99 falls. The HP-TCP profiles at `edec7937` show
zero warmed connects, one batch-thread creation, 13.2/14.9 futex calls and
7.4/7.8 epoll waits per batch on stable/1.85. Pattern generation occupies
68%/78% of user-stack samples and is outside timing; no shared control-worker
frame reaches 0.5%. There is no measured extra payload work or connection
setup to remove. Wake timing and VM/cache scheduling can change the median
without more CPU work; these profiles do not isolate their contribution or
rule out a shared control-worker effect. The watch item is retained, not
declared fixed. This PR changes no HP-TCP owner; its own paired run records
READ p50 changes of −3.6%/−17.8%, also with overlapping ranges.

## Ext4 tail diagnosis

[Run 42521765043](https://github.com/OrbitInfra/OrbiTE/actions/runs/42521765043)
profiles `7e40c7b`, with production sources identical to `4f6bd35c`, against
frozen TENT `e1ea5d31`. Each toolchain runs 1,000 byte-checked samples plus
three warmups for twenty implementation/filesystem/direction/shape cases.
Both four-vCPU Ubuntu 24.04 runners use Linux 7.0.0-1017-azure; stable
(Rust 1.99.1) runs on Xeon Platinum 8573C, and Rust 1.85 on EPYC 7763.
They are different hosts; do not pool their absolute times.

`benches/run.py --tail-profile-only` records monotonic sample windows and
system-wide io_uring, block-I/O, thread-creation and scheduler tracepoints.
The collector writes to tmpfs and copies evidence to ext4 only after each
benchmark exits. An earlier exploratory recording shared the measured ext4
device and is excluded from the figures here. Raw `perf` data, kernel event
formats, commands and sample windows remain in `cpu-bench-*` artifacts.
Run `python3 benches/analyze_io_tail.py "$OUT/orbite-profiles/tail"` to produce
per-sample submit/first-CQE/last-CQE/return, worker/caller wake and block timelines.
It rejects missing ring/CQE receipts and incomplete scheduler pairs. Block
events have no SQE identity: the analysis pairs exact device/operation/sector/
length receipts issued by the measured process and reports unmatched events.
That is correlation, not an invented per-SQE block receipt.

The table describes **diagnostic traces**, not benchmark performance. Each
cell is the median within the slowest ten samples of a 1,000-sample case.
CQE timestamps are kernel publication, not userspace consumption.
Issuer runnable time covers observed wake/preemption intervals after its first
scheduling; initial thread creation/scheduling is included in before-first-SQE
time and the fork/switch timeline. Block completion means the kernel event,
not a timestamp from physical hardware.

| 64 × 4 KiB gapped | Batch µs | Longest paired block I/O µs | Issuer blocked / runnable µs | Last block completion → last CQE µs | Last CQE → return µs |
|---|---:|---:|---:|---:|---:|
| Stable OrbiTE READ | 6,361 | 5,758 | 5,685 / 30 | 2.8 | 266 |
| Stable OrbiTE WRITE | 5,169 | 1,993 | 4,250 / 62 | 18.0 | 234 |
| Stable TENT READ | 8,094 | 2,027 | 7,118 / 86 | 3.1 | 994 |
| Stable TENT WRITE | 6,532 | 3,890 | 5,570 / 77 | 16.3 | 707 |
| Rust 1.85 OrbiTE READ | 27,175 | 26,288 | 25,866 / 18 | 9.7 | 255 |
| Rust 1.85 OrbiTE WRITE | 21,679 | 20,786 | 20,556 / 19 | 43.1 | 265 |
| Rust 1.85 TENT READ | 2,657 | 1,605 | 1,559 / 90 | 13.9 | 426 |
| Rust 1.85 TENT WRITE | 2,439 | 1,305 | 1,233 / 65 | 20.0 | 355 |

All 64 block issues pair with completions in every measured 64-block sample.
The longest delays therefore precede device completion, not owner cleanup.
For example, stable READ sample 697 lasts 8,691 µs: first SQE at +156,
first CQE at +628, final block completion at +8,396 and final CQE 1.7 µs
later. Worker wake-to-run never exceeds 6.6 µs; caller return follows the
last CQE by 293 µs. On 1.85, eight-block gapped/contiguous READ slow-tail
medians are 1,332/1,047 µs, versus TENT's 1,328/1,687 µs. These traces
locate a storage-side wait but do not distinguish virtual-device service,
host scheduling or buffered-reset/direct-I/O coherence as its underlying
cause. Nor do they explain why the implementations differ on the EPYC run.
The tail is **not declared fixed**; no production tuning is justified by this
evidence alone.

The proposed engine explanations can be narrowed:

- Every measured gapped batch uses **one ring, 64 SQEs and one submitting
  enter** in both implementations. Stable OrbiTE adds a median five
  zero-submission waits; 1.85 adds zero READ / one WRITE. TENT polls the mapped
  CQ. These waits never resubmit an SQE.
- All 64-block CQEs are published on the submitting batch worker. There is
  no caller-to-worker ring handoff in this case. Worker drain has **zero
  nanosleeps**; the benchmark caller still uses its documented status backoff.
- Aligned gaps allocate **no bounce buffer**. The new
  `aligned_file_batches_use_one_ring_without_bounce_for_gaps_or_contiguous_ranges`
  control fails every bounce allocation and checks one ring, full bytes and
  guards for READ/WRITE, 8/64 blocks, contiguous/gapped, ext4/tmpfs.
- OrbiTE creates **one batch worker** per multi-request submission; TENT
  creates none. Before-first-SQE time in OrbiTE's slow 64-block samples is
  152–171 µs, much shorter than the device wait. Removing thread creation
  cannot by itself explain or eliminate the measured millisecond tail.

Small tmpfs WRITE remains a separate cost: 994/1,000 stable and 992/1,000
1.85 samples create a batch worker after caller submission, versus zero for
READ and TENT. Diagnostic WRITE medians are 123/229 µs versus TENT's 58/111 µs;
these are not replacements for the untraced S26 numbers. Together with the
earlier `pthread_create`/`Batch::start_worker` stacks, this identifies the
pending-completion handoff. A later candidate is one short, deadline-capped
caller CQ wait for the already eligible small request, then handing the same
owner to the worker if still pending. Expiry would remain a handoff trigger,
never a release receipt. This needs a separate submission-latency/lifetime
review and measurement; no wait, pool or new worker is introduced here.

The [paired control run](https://github.com/OrbitInfra/OrbiTE/actions/runs/42522826931)
uses `7e40c7b` → `23f6911` on each toolchain: identical production sources and
workload hashes, with the ordinary S26 three trials, 100 samples and byte checks.
All 480 rows per side, including every slower row, go in the
[PR's complete tables](https://github.com/OrbitInfra/OrbiTE/pull/184).
Reproduce them with `benches/compare.py BEFORE AFTER --changed-profile none`.
All payloads are marked unchanged. Differences in this control pair are
run/build-placement variation, not an optimization result; comparison with
unchanged TENT bounds the interpretation without claiming statistical
equivalence or dismissing a large regression. Raw results remain outside Git.

## CPU fault matrix

[tests/faults/matrix.tsv](https://github.com/OrbitInfra/OrbiTE/blob/main/tests/faults/matrix.tsv)
is the executable fixture list: backend, fault, exact production-owner control
and expected charge/release state. It reuses the existing socket, file, mapping
and fallible-table controls. New TCP and HP-TCP cases kill a real requester
process after a partial WRITE. TCP leaves the destination untouched; HP-TCP
keeps the received prefix. Both retain the registration during unregister and
close, then verify all bytes and guards after physical socket drain.
The restart fixture keeps its reserved endpoint and bounds only setup-time
`AddrInUse` retries: concurrent process creation can inherit a listener until
exec closes it. Transfer deadlines, epoch rejection and drain assertions stay
unchanged.

The matrix covers peer death, timeout, cancellation, partial submission,
unregister/close during work, and restart authority. It distinguishes native
provider failures from injected completion faults. Fabric uses the real
`tcp;ofi_rxm` provider; its held/error CQ controls wrap that provider. io_uring
uses real files/rings. A missing kernel/provider fails, never silently skips.
SHM's synchronous memcpy has no native cancel or partial-post queue; queued
cancellation belongs to the common batch owner. Local files have no peer epoch:
path replacement is tested against the admitted inode. Those dispositions are
explicit, not counted as passes.

```bash
cargo test --locked --features io-uring,fabric --lib --no-run --message-format=json > "$OUT/build.json"
# BINARY is the compiler-artifact executable in build.json.
ORBITE_EXT4_DIR="$EXT4_DIR" ORBITE_TMPFS_DIR=/dev/shm \
  python3 tests/faults/run.py --binary "$BINARY" --output "$OUT/faults" --kill-repeats 50
```

CI executes the matrix on both toolchains with the full feature set. It repeats
each new process-kill control 50 times. Existing native fault/stability gates
remain in the matrix. This prepares P10; it does not close its installed
hardware acceptance matrix.

## Installed fault fixtures

The [verification queue](qualification.md#verification-queue) owns the exact
commands and hardware leases. Reuse the installed RDMA fault runner's
`installed-inflight-dma`, `installed-lost-completion`,
`installed-cleanup-failure` and restart modes: accepted posts remain charged
until matching retirement receipts, and failed cleanup retains the native
owner. The NVLink grouped requester-exit and graceful-close modes check
cooperative CUDA import/event lifetime and guarded bytes. Exporter crash and
physical device loss remain explicit limits, not simulated passes.

The additional `gds-p10` packaged consumer runs one real native READ while a
small, test-only cuFile observer withholds status receipts. `drain` checks a
wait timeout and concurrent unregister/file close, then releases real CQEs and
verifies the full payload and guards. `quarantine` withholds receipts past the
submission deadline plus destruction window and checks `Unquiesced`, retained
byte charges and GPU backing after every caller handle is dropped. It never
fabricates a completion or retries a
WRITE. The driver requires compatibility disabled, resource locks and matching
nvidia-fs READ-counter evidence. Both modes are written and compiled in CI,
**not run**. These are controlled receipt faults, not spontaneous device faults.
