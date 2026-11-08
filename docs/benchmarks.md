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
physical two-host, GPU, RNIC or SSD performance claim follows. Storage latency,
ext4 tails and any remaining bookkeeping cost stay separate work.

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
