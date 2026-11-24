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
direct path. Both harnesses open and preallocate the file once, reset it in
place with `pwrite`, and finish `fdatasync` before the timing window. Buffered
reset keeps verification simple without leaving dirty reset pages or allocation
work for the measured direct I/O. Payload, gap and guard checks are unchanged.
These are warmed measurements, **not cold SSD throughput**. Timed WRITE
completion still excludes a durability barrier. Maximum payload per batch is 64 MiB; the
allocation and guard extent is bounded separately.

On a Linux runner with the native dependencies from the
[CPU oracle](cpu-oracle.md), install GNU `time` and `liburing-dev` **before** building
TENT. Enable io_uring in the kernel and choose writable ext4 and tmpfs paths:

```bash
python3 tests/reference/run.py --fetch-reference --output "$OUT/reference"
python3 benches/run.py --reference "$OUT/reference" --output "$OUT/sweep" \
  --ext4 "$EXT4_DIR" --tmpfs /dev/shm --workers 2 --warmup 3 \
  --samples 100 --trials 3
```

Both output directories must be fresh and outside the checkout. The
`Frozen TENT CPU comparison` CI workflow runs this on stable and Rust 1.85 and
uploads `cpu-bench-*` artifacts. GPU, RDMA and GDS are emitted as
**not run (hardware)**, without estimated numbers. The scoped summary lives in
[qualification](qualification.md#cpu-benchmark-evidence).

## CPU workload thresholds

Each row is one profile, direction, block size, batch size and layout. Score
READ and WRITE separately, including every single size and both layouts of
the 8- and 64-block batches. The stable and Rust 1.85 toolchain runs each
produce their own verdicts. The existing P11 ratios are fixed for every row.

| CPU workload | In-run reference | Revision regression baseline |
| --- | --- | --- |
| CPU memcpy, ordinary TCP | Frozen TENT v0.3.14, same case | Frozen OrbiTE, same case |
| io_uring on ext4 and on tmpfs | Frozen TENT v0.3.14 on the same mount and route settings | Frozen OrbiTE on the same mount |
| HP-TCP on two loopback rails, SHM on the same host | OrbiTE is the only measured implementation | Frozen OrbiTE, same case |

Freeze TENT at `e1ea5d31bb5e46ff2df9f90e7c2fbaf52fcdee8c` and the
OrbiTE source baseline at `12d1e83964a596d9b578e9d2879b5cd78e776da6`,
the reviewed revision preceding this policy. A new candidate remeasures those
sources. Historical absolute timings and the latest moving main are unsuitable
denominators. Changing this baseline or a threshold requires a reviewed policy
change. The two frozen sources yield **64 TENT comparisons and 96 OrbiTE
comparisons per toolchain**.

| Metric per workload | Candidate / baseline, inclusive | Measurement per independent trial |
| --- | --- | --- |
| Verified goodput | ≥ 0.95 | Total verified payload bytes / sum of timed batch wall times, in GiB/s |
| p99 batch latency | ≤ 1.05 | Nearest-rank p99 of 100 timed batches, in ns |
| Process CPU per verified byte | ≤ 1.10 | Sum of whole-process CPU ns during the timed batches / total verified payload bytes |
| Peak resident memory | ≤ 1.10 | GNU `/usr/bin/time -f %M` on one fresh Linux benchmark process, in KiB. Includes both endpoints where present, setup, buffers, verification and teardown. [GNU Time manual](https://www.gnu.org/software/time/manual/time.pdf) |

Compute each metric inside each of three process trials, then divide the
candidate median by the baseline median. Compare the unrounded ratio. Each
row must meet all four limits. p50 and end-of-workload thread count remain
diagnostics. The Rust engine must also produce the expected owner count and
file route, and both drivers must verify every byte and guard. Correctness
failures invalidate the measurement.

Run both revisions sequentially on the same Linux boot, with the same
compiler, CPU configuration, affinity, mounts, worker settings, workload
source and measurement driver. Start the frozen OrbiTE sweep first. Each
sweep alternates OrbiTE/TENT trial order, using one caller, two workers per
engine and three warmup batches. Both use the warmed file and timing window
defined above. A timed file WRITE ends at I/O completion, before a durability
barrier. A traced or paced run cannot supply these samples.

For revision comparisons the measurement checkout supplies the Rust caller.
When a selected engine checkout has a different `benches/transfer.rs`, the
driver builds its locked engine with the measurement caller and checks rustc's
dependency file against the resulting executable. The receipt includes both
caller hashes. The build cache is keyed by the compiler wrapper's content,
the engine revision and the caller hash. A reused executable must match all three.
This keeps changes in benchmark allocations out of the engine-to-engine ratio.

To collect both sides in CI, dispatch `Frozen TENT CPU comparison` with
`baseline_ref` set to the full frozen OrbiTE revision above. To enforce the
limits as a job, use `enforce_cpu_guards=true` on a manual dispatch, which
also selects that baseline. The workflow archives all verdicts in
`orbite-bench/cpu-guards.json`. For local measurement, create a separate
clean worktree at that revision and run the command in **CPU sweeps** twice
from the candidate checkout. Add `--orbite-source "$BASELINE_WORKTREE"` for
the first run, use distinct output directories, and reuse the reference build
and mounts. To decide from downloaded bundles, keep their stdout/stderr files
alongside each manifest and run:

```bash
python3 -B benches/guards.py "$AFTER" --baseline "$BEFORE" --output "$OUT/cpu-guards.json"
```

The strict command exits **0** only when every comparison passes. **1** means
a complete pair crossed a threshold. A miss against an earlier OrbiTE revision
is a regression signal in this run. If both sweeps used the very same revision,
the report records same-revision variation and still exits 1. A miss against frozen TENT is a reference
target gap, which can already exist on the baseline revision. **2** means
evidence is incomplete or invalid, including missing RSS, mismatched hosts
or configurations, or a missing trial. A candidate-only report still gives
64 TENT comparisons and marks all 96 revision comparisons missing.
`--report-only` archives measured misses or an absent optional baseline;
malformed evidence still fails the job. Earlier archives lacking peak RSS
remain usable with the descriptive `compare.py` report.

Keep every crossing visible. Hosted clock, scheduler and virtual-disk
variation can move a median even with identical source. A rerun may inform
attribution and must retain both reports. Dispatch the same workflow with
`repeat_baseline=true` for two complete sweeps using the same frozen OrbiTE
checkout and binary on each runner. Both sweeps use independent processes and
also measure frozen TENT. The guard report labels baseline crossings in this
control `same-revision-variation` and rejects a mismatched OrbiTE binary hash. Virtual ext4 measures this warmed
guest workload. Physical NVMe tails, mixed load, faults and Python API
correctness have separate gates.

### Buffered small WRITE groups

In the [pre-change
comparison](https://github.com/OrbitInfra/OrbiTE/actions/runs/44861551257), stable and Rust
1.85 crossed the same metric on 5 of 96 OrbiTE baseline workloads and 30 of 64 matched TENT
workloads. For warmed tmpfs WRITE at eight gapped 4 KiB offsets, OrbiTE delivered 29% of
TENT goodput on both toolchains and used about 2.9 times its CPU per byte.

The [identical-binary repeat](https://github.com/OrbitInfra/OrbiTE/actions/runs/44868059481)
produced 20/96 self-baseline crossings on stable and 18/96 on Rust 1.85. Of the 5 shapes
that crossed on both toolchains, 4 shared a metric. All five earlier self-baseline signals
fell inside their limits on both toolchains. Of the 30 earlier TENT gaps, 28 reproduced at
least one common metric. Each runner used the same OrbiTE and TENT binary hashes for both
sweeps, with fresh processes and independent sweep IDs.

The [same-runner comparison](https://github.com/OrbitInfra/OrbiTE/actions/runs/44868978517)
pairs frozen OrbiTE with the buffered-WRITE change at `625f0ed5`. The table shows the
medians of three trials for the target workload. Columns give frozen OrbiTE → candidate,
followed by frozen TENT from the candidate sweep in parentheses.

| Rust | Verified GiB/s | p99 µs | CPU ns/byte | Peak RSS KiB |
|---|---:|---:|---:|---:|
| stable | 0.179 → 0.366 (0.367) | 219.5 → 157.3 (164.0) | 6.68 → 3.01 (3.17) | 3760 → 3892 (18616) |
| 1.85.0 | 0.143 → 0.318 (0.287) | 318.8 → 223.7 (214.1) | 8.20 → 3.41 (4.00) | 3596 → 3568 (18660) |

Both toolchains met all four TENT targets for the gapped WRITE. In the same paired run, five
other workloads crossed a common self-baseline guard. Ext4 WRITE goodput crossed for a
single 4 KiB block and eight gapped 4 KiB blocks. The single 4 KiB tmpfs READ and WRITE and
the SHM contiguous 8 × 4 KiB READ crossed the p99 guard. These signals stay open for
follow-up. The [singleton tmpfs retest](#singleton-tmpfs-tail-latency) revisits the two tmpfs rows.

Independent Linux aarch64 tracing counted one batch-worker start per target batch before the
change and zero after, with one ring setup in both cases. The target kept all eight physical
owners. The 64-request control still started a worker. The [full crossing
tables](https://github.com/OrbitInfra/OrbiTE/pull/221) retain results for the other CPU
cases. See [file submission and drain](storage.md#dram-transfers-with-io_uring) for bounds,
incomplete CQ sets and worker failure.

### Eight contiguous buffered READs

This case reads eight adjacent 4 KiB ranges on a warmed tmpfs file. Each
request has its own destination, SQE and CQE. The caller resets and syncs
the file before timing and verifies every byte and guard after the batch.
The [paired diagnostic](https://github.com/OrbitInfra/OrbiTE/actions/runs/45089346998)
compares parent `1381294` and candidate `1915229` with the same Rust caller and frozen
TENT binary on the same boot. Each toolchain runs six fresh processes per
implementation and revision, alternating launch order, with three warmups,
1,000 measured batches, one caller and two configured workers per engine.

Run `benches/run.py --tail-profile-only tmpfs-read-batch` with the reference,
output and mount arguments from the [CPU sweeps](#cpu-sweeps). Separate
`perf` captures record the fork of the batch worker, the first context switch
onto that worker, all eight kernel CQEs and the caller's status loop.
The fork-to-first-schedule interval measures scheduler handoff. Polling runs
from the first status call through observation of terminal status and can
overlap worker execution. The analyzer intersects the caller's nanosleep
syscall intervals with the time after the final CQE. The six timing trials
per implementation run with tracing disabled.
Recompute the phase table from either extracted `tail/` directory with
`python3 benches/analyze_io_tail.py /path/to/tail`. It matches each READV to
its 4 KiB CQE by request pointer and user data before attributing a time.

Before the change, each of the 1,000 measured batches on each toolchain
started a worker and the caller entered nanosleep. The following values
are medians in microseconds from the scheduler and io_uring traces. The
last column uses the ten slowest end-to-end windows on each host.

| Original OrbiTE | Worker fork to first run | Caller status loop | In nanosleep during loop | Last CQE to terminal observation | After last CQE inside nanosleep, slowest 1% |
|---|---:|---:|---:|---:|---:|
| Stable | 9.28 | 131.91 | 122.67 | 74.44 | 100.68 |
| Rust 1.85 | 9.25 | 74.41 | 66.90 | 26.04 | 112.48 |

In the same before captures TENT issued all eight reads on the caller. Its
median status loop was 1.03 / 20.35 µs and terminal observation followed
the last CQE by 5.26 / 1.51 µs on stable / Rust 1.85. OrbiTE's original path
also took 72.05 / 48.79 µs from batch start to the first SQE, including file
preparation and the worker's first schedule. With caller submission this
interval fell to 32.12 / 15.11 µs. All 2,000 candidate groups kept one ring
and eight separate 4 KiB completions. They started zero workers, entered no
polling sleeps and observed completion with one status call. This is the
ready-CQE case. The real-ring controls separately force the bounded wait to
hand an incomplete group to a worker.

Untraced values below are medians across six processes per cell. Both TENT
columns show the same executable measured in each phase. Cloud tail latency
varied between phases, including on this control.

| Rust | OrbiTE GiB/s, before → after | TENT GiB/s, before → after | OrbiTE p99 µs, before → after | TENT p99 µs, before → after | OrbiTE CPU ns/byte, before → after |
|---|---:|---:|---:|---:|---:|
| Stable | 0.261 → 0.552 | 0.364 → 0.373 | 211.85 → 174.75 | 168.01 → 176.44 | 4.135 → 1.715 |
| Rust 1.85 | 0.311 → 1.291 | 0.681 → 0.671 | 171.10 → 49.60 | 188.98 → 133.75 | 3.204 → 0.730 |

The [first pair](https://github.com/OrbitInfra/OrbiTE/actions/runs/45086639095)
and an [independent runner pair](https://github.com/OrbitInfra/OrbiTE/actions/runs/45087715092)
also observed zero worker starts and zero caller sleeps on the candidate
before the short-submit ownership fix. Across those runs, untraced candidate
p99 ranged from 47 to 252 µs. The final stable runner reached roughly the
same median p99 as TENT while delivering 1.48 times its goodput. In two
of its six candidate-phase trials, OrbiTE p99 exceeded TENT p99 by over 5%.
The [full CPU sweeps](https://github.com/OrbitInfra/OrbiTE/actions/runs/45089330744)
use the standard 100-sample trials, compare with the fixed OrbiTE regression
baseline and retain all TENT workload verdicts. The results above apply to
one caller reading a warmed tmpfs file.

### Eight contiguous direct READs

This workload reads eight adjacent 4 KiB blocks from a preallocated ext4
file through a direct descriptor. The [same-boot comparison](https://github.com/OrbitInfra/OrbiTE/actions/runs/45118467752)
used parent `e4a12a2`, candidate `27bfe46` and frozen Mooncake TENT
`e1ea5d31`. Each toolchain ran on its own x86-64 Linux guest. Both phases
used the same caller, kernel, mounts and TENT binary on that guest.
Run `benches/run.py --tail-profile-only ext4-read-batch` with the reference,
output and mount arguments from the [CPU sweeps](#cpu-sweeps). Each of six
fresh processes per implementation and phase ran three warmups and 1,000
measured batches, alternating implementation order. File reset and
`fdatasync` preceded timing. Both callers checked each batch's byte count,
payload and guards. The separate traces matched eight READVs to eight
independent completions in every measured batch.

The table gives the median of the **untraced** process trials. Goodput counts
32 KiB per batch. CPU time includes all threads in the process and p99 covers
the full batch. The TENT columns use its candidate-phase trials.

| Rust | OrbiTE GiB/s, before → after | TENT GiB/s | OrbiTE CPU ns/byte, before → after | TENT CPU ns/byte | OrbiTE p99 µs, before → after | TENT p99 µs |
|---|---:|---:|---:|---:|---:|---:|
| Stable | 0.189 → 0.393 | 0.513 | 3.248 → 1.355 | 1.833 | 202.90 → 132.99 | 122.51 |
| Rust 1.85 | 0.190 → 0.389 | 0.509 | 3.246 → 1.379 | 1.847 | 204.16 → 136.71 | 115.80 |

Separate captures verify a successful `O_DIRECT` open on the payload and
match every READV to its full 4 KiB CQE. For each issuing block device the
analyzer matches the queued data sectors to issued requests and completions,
including requests merged or split by the kernel. The span runs from first
issue to last completion on the guest. Worker handoff runs from fork to
first scheduled execution. Caller polling runs from the first public status
call to the terminal observation. These intervals overlap. Recompute the
distributions with `python3 benches/analyze_io_tail.py /path/to/tail`.

| Trace median, µs | Stable OrbiTE, before → after | Stable TENT | Rust 1.85 OrbiTE, before → after | Rust 1.85 TENT |
|---|---:|---:|---:|---:|
| Batch start to first SQE | 49.97 → 18.62 | 23.89 | 49.27 → 19.35 | 23.94 |
| Guest block issue to completion | 32.93 → 35.62 | 21.82 | 32.74 → 36.15 | 21.87 |
| Worker handoff | 9.18 → 0 | 0 | 9.32 → 0 | 0 |
| Caller polling | 143.63 → 0.10 | 26.41 | 143.10 → 0.09 | 26.47 |
| Last CQE to terminal observation | 56.51 → 15.76 | 1.38 | 56.82 → 16.17 | 1.43 |

The parent started a worker and entered polling backoff in every measured
batch. Even after the last CQE it spent a median 55.21 / 55.42 µs asleep
on stable / Rust 1.85. The bounded caller path completed all 1,000 groups
per toolchain before the first status call. It retained eight independent
SQEs, CQEs and byte counts. On the stable candidate, a gap in the underlying
sector mapping split each group into 4 KiB and 28 KiB requests on one
guest device. The other seven captures issued one 32 KiB request per group.
The candidate protected each destination against a concurrent
truncate by copying only its CQE prefix from private storage.

On these hosts OrbiTE reached 77% of TENT goodput, used 25–26% less CPU
per byte and took 9–18% longer at p99. Both the guest block span and the
time between the final CQE and the public receipt still exceeded TENT.
Part of the receipt gap reflects when each implementation closes the ring.
OrbiTE's caller performed two unmaps and one close before reporting terminal
status. TENT performed them during batch free. From the last CQE to the
end marker recorded after free, the medians were 17.32 / 17.91 µs for
OrbiTE and 11.31 / 11.24 µs for TENT. This includes the intervening harness
work. The untraced latency and goodput above include free for both.
The [CQ polling experiment](https://github.com/OrbitInfra/OrbiTE/actions/runs/45119467264)
added up to 16 µs of active polling before the bounded sleeping wait on the
same protected implementation. Goodput moved from 0.378 to 0.351 GiB/s
on stable and from 0.374 to 0.468 on Rust 1.85. P99 moved from 150.68 to
152.52 µs and from 138.50 to 136.03 µs, respectively. Process CPU per
byte rose 45% and 41%, exceeding the 10% allowance on both hosts, so the
caller keeps the sleeping wait. The [full CPU sweeps](https://github.com/OrbitInfra/OrbiTE/actions/runs/45118496460)
compare all shapes, including single 64 MiB direct READs, with the fixed
OrbiTE baseline and frozen TENT using three 100-batch trials per shape.
Guest storage varied between hosts. In the [original Azure capture](https://github.com/OrbitInfra/OrbiTE/actions/runs/45116563186)
the ten slowest Rust 1.85 parent batches spent a median 13.07 ms of their
13.42 ms at the block device. Physical NVMe latency remains a separate
qualification.

### Large direct READs

A buffered reset and `fdatasync` precede each measured 64 MiB single READ
on ext4. The public caller checks the payload, destination guards, byte
count and direct descriptor. Six fresh processes per engine version cover
every run order, with three warmups and 30 samples each. Each row gives the
median of the six untraced process trials. The fixed baseline is
`12d1e839`. The protected engine receives the direct READ into private
storage, then copies the CQE-confirmed prefix to the caller. The last
entry in each group uses 2 MiB-aligned private storage with transparent
huge-page advice. [Matched public-engine trials](https://github.com/OrbitInfra/OrbiTE/actions/runs/45130293237)

| Rust | GiB/s, baseline / protected / advised | CPU ns/byte, baseline / protected / advised | p99 ms, baseline / protected / advised | peak RSS MiB, baseline / protected / advised |
|---|---:|---:|---:|---:|
| Stable | 6.24 / 2.01 / 2.52 | 0.145 / 0.471 / 0.341 | 11.64 / 32.49 / 28.44 | 323.4 / 323.5 / 323.5 |
| 1.85 | 6.52 / 2.10 / 2.55 | 0.140 / 0.453 / 0.331 | 11.81 / 31.13 / 27.94 | 323.5 / 323.5 / 323.5 |

Separate native READV controls measured about 16,385 minor faults per
64 MiB read. Huge-page advice reduced that count to about 34. A full CQE
still required one copy of the entire payload. The matched engine exceeded
its CPU and p99 limits and missed its goodput floor on both toolchains.
Peak RSS remained within its limit. [Page-fault and copy controls](https://github.com/OrbitInfra/OrbiTE/actions/runs/45128204113)

A single 64 MiB direct READ needs 64 MiB of private storage through its
CQE. A separate Linux aarch64 guest with 3 vCPUs measured the parent, the
protected default and the protected engine with one reusable 64 MiB slot.
Each of the six possible launch orders ran once, with a fresh process,
three warmups and ten byte-checked reads per condition. Median process CPU
per READ was 0.431, 3.320 and 2.428 ms, respectively. Reuse lowered CPU
in all six pairs and raised the process peak from 323 to 387 MiB. It
retains the slot between operations. This experiment measures the combined
cost of allocation, copying and completion on that guest. The staging pool
remains opt-in. [Review and measurement conditions](https://github.com/OrbitInfra/OrbiTE/pull/226)

### Singleton tmpfs tail latency

The `tmpfs_tail_only` input of the native CPU workflow measures warmed, buffered
io_uring READ and WRITE of one contiguous 4 KiB block. On each Linux runner,
A/A launches the frozen OrbiTE binary twice. A/B launches that same binary and
the candidate. Frozen TENT runs alongside each pair. The driver uses all six
launch orders twice per operation and comparison, reversing the order of the
cases on the second pass. Each of the 144 processes has one caller, two configured
workers, three warmups and 1,000 untraced samples with byte checks. Each toolchain
has its own runner and boot.

`tmpfs-tail.json` records each process's nearest-rank p99. It also reports p99
over the first 100 samples for comparison with the formal sweep's sample count.
Those samples belong to the same process as the longer series. The paired
ratio is the median of twelve probe/baseline ratios, with six pairs in each
launch direction. The report also gives the probe/TENT ratio and the count of
pairs above 1.05. Use the A/A results and both launch directions to interpret
a crossing. The [formal thresholds](#cpu-workload-thresholds) continue to use
the complete paired sweeps.

[Run 1](https://github.com/OrbitInfra/OrbiTE/actions/runs/44972155456) and
[run 2](https://github.com/OrbitInfra/OrbiTE/actions/runs/44972237044) tested
`9c4ea6a` against frozen OrbiTE `12d1e839` and the pinned TENT build.
Stable was Rust 1.100.0. Run 1 used Xeon 6975P-C for stable and EPYC 9V74 for
1.85.0. Run 2 used EPYC 7763 for stable and Xeon 6975P-C for 1.85.0.
All 576 fresh processes verified their 1,000 measured batches. The table gives
paired p99 ratios over those samples. The launch-order column splits A/B by
which OrbiTE process started first.

| Run | Rust | Op | A/A | A/B | Baseline first / candidate first | A/B pairs above 1.05 | Candidate/TENT |
|---|---|---|---:|---:|---:|---:|---:|
| 1 | stable | READ | 1.012 | 1.019 | 1.086 / 0.895 | 4/12 | 0.221 |
| 1 | stable | WRITE | 1.229 | 0.943 | 1.413 / 0.816 | 5/12 | 0.798 |
| 1 | 1.85.0 | READ | 1.120 | 0.996 | 0.861 / 1.038 | 5/12 | 0.532 |
| 1 | 1.85.0 | WRITE | 1.055 | 0.889 | 0.840 / 0.927 | 3/12 | 0.821 |
| 2 | stable | READ | 1.090 | 0.888 | 0.888 / 0.903 | 4/12 | 0.726 |
| 2 | stable | WRITE | 0.955 | 1.015 | 1.018 / 1.011 | 3/12 | 0.962 |
| 2 | 1.85.0 | READ | 0.942 | 1.002 | 0.973 / 1.042 | 4/12 | 0.211 |
| 2 | 1.85.0 | WRITE | 0.996 | 0.945 | 0.945 / 0.947 | 4/12 | 0.667 |

The earlier singleton p99 crossings in the full sweep did not recur in any
of the eight 1,000-sample A/B medians. Four of the eight same-binary medians
and some individual A/B pairs still crossed.
For example, stable WRITE in run 1 gave 1.413 with the baseline first and
0.816 with the candidate first. The same-binary WRITE control gave 1.229.
Taking only the first 100 samples changed run 1 stable READ from 1.019 to
1.274 and run 2 stable WRITE from 1.015 to 1.170. These measurements support
retaining the current single-request submission path. The ext4 WRITE goodput
and SHM READ p99 cases from the full sweep remain open.

## Peak memory for a 64 MiB file READ

The workload reads one aligned, contiguous 64 MiB block from a preallocated
tmpfs file into registered DRAM. The driver requires the tmpfs mount and
OrbiTE's buffered route with one physical owner. Each caller still resets
and syncs the file before timing and reads the entire file back after drain.
Both check the payload, guards and unused registration. Rust expects that
registration to contain the sentinel throughout. TENT seeds its remote
registration with the input pattern on READ and checks that pattern afterward.

The earlier Rust caller held a separate 64 MiB all-sentinel image for the
unused registration. A Linux aarch64 phase measurement recorded 264,964 KiB
RSS at the 67,108,864-byte CQE with no bounce buffer, then 265,148 KiB
after drain. The whole-file verification read reached 396,244 KiB and the
subsequent copy used to check the unused registration reached 396,248 KiB.
In separate processes, changing only the file readback to 64 KiB chunks
gave 396,124 KiB versus 396,052 KiB for the instrumented control.

The Rust verifier now checks the complete unused registration against the
sentinel without materializing its expected contents. Four fresh untraced
processes per revision on the same aarch64 boot, in counterbalanced order,
measured 396,044 → 330,442 KiB median peak RSS (three warmups and 100 READs
per process). Both callers retain the full expected file image and one bulk
readback. File and registration corruption controls check the first and last
bytes, payload and gaps, plus a truncated or extended file.

The native CPU workflow can repeat the comparison with frozen TENT on
stable and Rust 1.85.0. The `cpu-bench-*` artifacts contain the independent
GNU time process peaks in `tmpfs-read-rss.json`. Separate short Heaptrack
runs supply `peak.stacks` and `timeline.massif`. On each toolchain the two
revisions used their own callers, the same TENT executable and the same Linux
boot. The table gives median peak RSS in KiB over three fresh processes per
cell (three warmups, 100 measured READs). The separate heap profiles show
six payload-sized allocations at the Rust peak before the change and five
afterward, a difference of exactly 67,108,864 live bytes. TENT had five
in both runs. [Paired profiles](https://github.com/OrbitInfra/OrbiTE/actions/runs/45035303646)

| Rust toolchain | OrbiTE before | OrbiTE after | TENT before | TENT after | OrbiTE / TENT before → after |
| --- | ---: | ---: | ---: | ---: | ---: |
| stable 1.100.0 | 396,720 | 331,256 | 345,164 | 345,228 | 1.1494 → 0.9595 |
| 1.85.0 | 396,828 | 331,280 | 345,260 | 345,116 | 1.1494 → 0.9599 |

The full regression comparison builds the same measured caller against both
OrbiTE engine revisions and records all four metrics per workload.
[Formal sweeps](https://github.com/OrbitInfra/OrbiTE/actions/runs/45035282699)

```bash
gh workflow run native-cpu.yml --ref main -f tmpfs_read_rss_only=true \
  -f baseline_ref=e4c4890f2e921ab61f555a56e58611bf6b237822
```

## TCP profile and connection reuse

### Process memory for a 64 MiB READ

The native CPU workflow's `tcp_read_rss_only` input measures one contiguous
64 MiB READ on a warmed loopback connection. Both engines live in one
process. For each implementation GNU time records peak process RSS in three
fresh trials with three warmups, 100 samples, one caller and two configured
workers per engine. Each sample checks the payload and both registration
guards. A separate short run records the live heap allocation stacks at the
global allocation peak and their timeline with Heaptrack. Heap bytes and
process RSS have different accounting.

With `baseline_ref`, this diagnostic uses the caller checked into each
revision. This lets a change in the caller itself be measured on the same
runner as the previous caller and frozen TENT. The `cpu-bench-*` artifacts
contain `tcp-read-rss.json`, the per-process GNU time receipts,
`peak.stacks` and `timeline.massif` for each implementation.

```bash
gh workflow run native-cpu.yml --ref main -f tcp_read_rss_only=true \
  -f baseline_ref=03452a0462dcbc5261a52a73f791d6e57bab283f
```

The original Rust caller kept six payload-sized allocations live at the
transfer peak. Two held the registered ranges, one held the expected bytes,
and one held an all-sentinel image used only to verify file transfers. The
TCP requester held the complete reply until commit and the source held its
snapshot while sending. At 64 MiB, each TCP buffer accounted for 64 MiB of
live heap. Frozen TENT also held two RPC payloads at its peak, at the
client receive site and in `ControlService::onRecvData`. Its caller verified
the registered bytes directly. The [original allocation stacks](https://github.com/OrbitInfra/OrbiTE/actions/runs/44977183125)
showed six large allocations for OrbiTE and five for TENT on both toolchains.

In this TCP comparison the Rust caller allocated the all-sentinel image only
for file workloads. The [file READ profile](#peak-memory-for-a-64-mib-file-read)
covers its subsequent removal. The verifier checks every byte after drain.
The table gives the median peak RSS in KiB over three untraced processes per
cell. Each toolchain ran both revisions on the same boot with the same TENT
executable. In the separate heap profiles, OrbiTE's peak live allocations
fell from 384.1 to 320.1 MiB on both toolchains, a decrease of exactly
67,108,864 bytes. [Paired RSS and allocation profiles](https://github.com/OrbitInfra/OrbiTE/actions/runs/44977519660)

| Rust toolchain | OrbiTE before | OrbiTE after | TENT before | TENT after | OrbiTE / TENT before → after |
| --- | ---: | ---: | ---: | ---: | ---: |
| stable 1.100.0 | 396,668 | 331,112 | 343,848 | 343,680 | 1.154 → 0.963 |
| 1.85.0 | 396,552 | 331,136 | 346,596 | 346,768 | 1.144 → 0.955 |

The [full revision comparison](https://github.com/OrbitInfra/OrbiTE/actions/runs/44978023506)
used the corrected caller for both OrbiTE engine revisions. OrbiTE used
0.9640× TENT's peak RSS on stable and 0.9644× on 1.85.0. Against the frozen
OrbiTE engine the ratios were 1.0003 and 0.9998. The same TCP READ delivered
0.886× and 0.877× TENT's goodput, with p99 at 1.133× and 1.094× TENT.
Both timing metrics missed their targets in this sweep and met them in the
[earlier sweep](https://github.com/OrbitInfra/OrbiTE/actions/runs/44868978517).
Against the frozen OrbiTE engine, stable p99 also exceeded the 1.05 limit
at 1.100×. On 1.85.0 the ratio was 1.035×.

An independent Linux aarch64 control split only the post-transfer byte check
into 64 KiB chunks and measured 395,968 KiB peak RSS, compared with
395,960 KiB for its diagnostic control. Splitting only the pre-transfer reset
gave 395,912 KiB. Changing only the lifetime of the file image gave
330,360 KiB. Each control used three fresh processes, three warmups and six
measured READs. The [interrupted READ control](cpu-oracle.md#interrupted-read-reply)
checks the destination after a partial reply and source loss.

### Connection reuse

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

The historical [ready-CQE pass](https://github.com/OrbitInfra/OrbiTE/pull/183)
reduced stable tmpfs 4 KiB READ p50 from 156 to 51 µs, but WRITE remained
193 µs versus TENT's 84 µs: one nonblocking check often missed the WRITE CQE
and started a batch worker. Its ext4 values used the old reset harness.

The [before profiles](https://github.com/OrbitInfra/OrbiTE/actions/runs/42691895658)
at `578531b7` and [after profiles](https://github.com/OrbitInfra/OrbiTE/actions/runs/42692901727)
at `a5874066` use the current preallocated, synchronized reset and bulk
verification. Before profiles use EPYC 7763; after uses EPYC 9V74 on stable
and EPYC 7763 on 1.85. These are separate diagnostic runs; the untraced
same-VM pairs below establish the timing comparison. Raw `perf` and `strace` output stays
in `cpu-bench-*` artifacts. Setup-subtracted calls per 4 KiB tmpfs batch are:

| Toolchain / operation | clone/clone3 before → after | futex before → after | io_uring_enter before → after |
|---|---:|---:|---:|
| Stable READ | 0.00 → 0.00 | 6.00 → 6.00 | 1.00 → 1.00 |
| Stable WRITE | 0.00 → 0.00 | 6.00 → 6.00 | 1.00 → 1.00 |
| Rust 1.85 READ | 0.00 → 0.00 | 6.00 → 6.01 | 1.00 → 1.00 |
| Rust 1.85 WRITE | 0.00 → 0.00 | 6.00 → 6.00 | 1.00 → 1.00 |

Both keep one ring setup per batch; the one lazy availability probe is outside
measurement. TENT likewise records no per-batch clone, one setup/enter and
approximately zero futex calls. Futex calls are not a handoff count. Tracing
delays the initial CQ check enough to hide worker creation even on the old
source, so these zero-clone traces cannot prove ordinary WRITEs avoided it.
The less intrusive before WRITE CPU profiles still attribute 1.75%/9.72% to
`pthread_create` on stable/1.85. Neither that frame nor `Batch::start_worker`
appears in the after
WRITE reports at the 0.5% cutoff. The 1.85 reports contain only 72 before and
118 after user-stack samples, so absence is not proof of zero calls. Its
separate `perf stat` WRITE sample changes p50/process-CPU from 197/193 to
86/97 µs; process context switches remain 253→225, including setup and warmup.
Blocking CQ waits still sleep and wake. These diagnostic runs do not replace
the untraced paired comparison.

The single-request change submits one contiguous host request of at most 64 KiB on an
idle batch, checks the CQ once, then waits up to 200 µs, capped by the request
deadline. At most three interrupted/unready waits share that window. A CQE
finishes through the normal prefix and admission owners. A missed window or
enter error moves the same ring to the existing drain worker without resubmission.
Runtime queues, background progress, eligible fallback, CUDA/pinned producer
waits and vectors use the worker. The later [buffered WRITE comparison](#buffered-small-write-groups) covers small groups. No ring pool,
thread pool or coordinator is added. The [owner controls](storage.md) preserve
preflight, partial-submit drain, exact short prefixes and quarantine. The
late-CQE tmpfs/ext4 control counts zero worker starts; disabling only the
wait makes it fail with one start. This is the deterministic handoff proof.

### Small-file paired results

[Run 42692901727](https://github.com/OrbitInfra/OrbiTE/actions/runs/42692901727)
completed **480 checked rows before and after on each toolchain**, comparing
`578531b7` with `a5874066`. Each pair shares one VM, compiler, ext4/tmpfs mounts,
workload hashes, reset and verification. Three trials, 100 samples, three
warmups, one caller and two configured engine workers are unchanged. Revision
order is before then after; implementation order alternates across trials.
The frozen reference is Mooncake v0.3.14 `e1ea5d31`. No diagnostic pacing is
used. Raw trials and profiles stay outside Git in the run's artifacts.

The complete [stable table](https://github.com/OrbitInfra/OrbiTE/pull/193#issuecomment-6752994908) and
[1.85 table](https://github.com/OrbitInfra/OrbiTE/pull/193#issuecomment-6752602865)
include all 160 workload/implementation cases per toolchain and every numerical
regression; each case aggregates three byte-checked trial rows. Reproduce with
`python3 benches/compare.py BEFORE AFTER --changed-profile io_uring-ext4 --changed-profile io_uring-tmpfs`.
CPU is process time per batch; the tables show medians of trial statistics.

Stable: four logical CPUs (AMD EPYC 9V74), Ubuntu 24.04, Linux
7.0.0-1017-azure, rustc 1.99.1 (58cf8f9c3 2026-10-13), GCC 13.3.0.

| Workload | OrbiTE p50 µs before → after | TENT p50 µs before → after | OrbiTE p99 µs before → after | OrbiTE CPU µs before → after |
|---|---:|---:|---:|---:|
| tmpfs, 1 × 4 KiB READ | 33.9 → 37.1 | 62.8 → 57.5 | 67.5 → 56.6 | 37.9 → 39.5 |
| tmpfs, 1 × 4 KiB WRITE | 117.9 → 57.3 | 59.7 → 56.3 | 196.7 → 151.9 | 130.2 → 61.5 |
| tmpfs, 1 × 64 KiB READ | 45.6 → 44.3 | 71.4 → 69.5 | 108.7 → 131.6 | 51.1 → 48.3 |
| tmpfs, 1 × 64 KiB WRITE | 128.1 → 70.0 | 132.9 → 127.0 | 193.9 → 166.8 | 148.6 → 73.6 |
| ext4, 1 × 4 KiB READ | 178.1 → 122.7 | 142.0 → 143.4 | 259.5 → 252.6 | 143.0 → 68.1 |
| ext4, 1 × 4 KiB WRITE | 200.2 → 144.9 | 197.0 → 199.7 | 275.6 → 434.3 | 182.7 → 85.1 |
| ext4, 1 × 64 KiB READ | 193.7 → 150.1 | 208.2 → 204.4 | 239.2 → 268.0 | 148.6 → 75.9 |
| ext4, 1 × 64 KiB WRITE | 218.6 → 193.7 | 206.9 → 205.6 | 331.0 → 544.0 | 213.4 → 102.5 |

Rust 1.85: four logical CPUs (AMD EPYC 7763), Ubuntu 24.04, Linux
7.0.0-1017-azure, rustc 1.85.0 (4d91de4e4 2025-02-17), GCC 13.3.0.

| Workload | OrbiTE p50 µs before → after | TENT p50 µs before → after | OrbiTE p99 µs before → after | OrbiTE CPU µs before → after |
|---|---:|---:|---:|---:|
| tmpfs, 1 × 4 KiB READ | 45.3 → 46.5 | 80.9 → 77.5 | 87.6 → 84.2 | 49.6 → 53.3 |
| tmpfs, 1 × 4 KiB WRITE | 192.8 → 76.1 | 80.1 → 79.3 | 325.8 → 177.0 | 181.3 → 86.6 |
| tmpfs, 1 × 64 KiB READ | 58.5 → 58.0 | 88.5 → 88.3 | 157.2 → 156.1 | 63.4 → 61.9 |
| tmpfs, 1 × 64 KiB WRITE | 207.8 → 99.2 | 111.4 → 112.4 | 276.8 → 197.2 | 199.1 → 106.9 |
| ext4, 1 × 4 KiB READ | 272.5 → 148.6 | 190.9 → 172.2 | 378.9 → 419.2 | 202.3 → 97.4 |
| ext4, 1 × 4 KiB WRITE | 282.8 → 171.5 | 186.6 → 191.0 | 422.3 → 439.5 | 246.5 → 116.2 |
| ext4, 1 × 64 KiB READ | 370.4 → 259.1 | 326.0 → 376.2 | 469.1 → 468.9 | 215.5 → 122.7 |
| ext4, 1 × 64 KiB WRITE | 342.0 → 232.7 | 352.7 → 320.1 | 527.8 → 464.9 | 267.2 → 140.9 |

Tmpfs 4 KiB WRITE p50 falls 51% on stable and 61% on 1.85; CPU falls 53%/52%.
After p50 is 1.02×/0.96× TENT, respectively. This removes the measured ordinary
small-WRITE handoff gap on these runs, not every conditional handoff.

Regressions remain, with every case reviewed in the linked tables:

- Stable ext4 64 KiB WRITE p99 rises **331→544 µs (+64.4%)**, with separate
  three-trial ranges (284–403→513–618 µs), while p50 and CPU fall. TENT's
  unchanged p99 also rises 805→1322 µs (+64.4%), with overlapping ranges.
  A CQE that misses the 200 µs window can pay that wait before worker handoff;
  this is a bounded latency tradeoff, not a uniform p99 improvement. The
  relative contribution of the wait and VM/device variation is not isolated.
- Other changed-path stable regressions are ext4 4 KiB WRITE p99 +57.6%,
  ext4 64 KiB READ p99 +12.1%, tmpfs 64 KiB READ p99 +21.0%, and tmpfs 4 KiB
  READ p50/CPU +9.3%/+4.3%. Their ranges overlap. TENT p99 moves +8.0%,
  +242.8% and −1.0% for the first three cases; TENT small READ p50/CPU moves
  −8.5%/−3.4%. Those reference movements do not explain away the regressions.
- On 1.85, ext4 4 KiB READ/WRITE p99 rises 10.6%/4.1%; tmpfs 4 KiB READ
  p50/CPU rises 2.6%/7.5%, with throughput down 6.6%. All those ranges
  overlap. TENT ext4 READ p99 is nearly unchanged and WRITE p99 rises 0.5%; its tmpfs READ
  p50/CPU moves −4.2%/−4.7%. Changed wake timing remains a possible cost.
- **101/160 stable and 95/160 1.85 cases** have at least one numerically worse
  statistic. Only five/three are eligible caller-wait cases. Larger file
  groups, TCP, HP-TCP, SHM, memcpy and TENT payload code is unchanged. Every
  slower case lists its trial-range overlap and the unchanged TENT movement
  where that baseline exists. Sequential-run cache, scheduling, VM and shared
  code-generation effects are not causally separated. Overlap is descriptive,
  never proof of noise or a reason to discard a row. No virtual-disk tail
  investigation or tuning is added here.

The two toolchain jobs are separate VMs and are not pooled. p99 uses only 100
samples per trial. The timed WRITE is not a durability receipt. These results
support only this CPU cloud runner and workload; no GPU, RNIC or physical-NVMe
gain is claimed. A request that misses the bounded window still needs worker
drain, and the virtual-disk tail remains a P11 hardware-batch limit.

### HP-TCP watch item recheck

[Run 42501030618](https://github.com/OrbitInfra/OrbiTE/actions/runs/42501030618)
separately repeats all 480 checked rows before and after the TCP-pool change,
`f0556448` → `edec7937`, with the same S26 parameters. Each pair shares one VM;
stable uses four EPYC 9V74 logical CPUs and 1.85 uses four EPYC 9V45 logical
CPUs. These are separate from the caller-wait pairs above and are not pooled.

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

The **old harness** truncated and rewrote the file through the page cache
before every sample without syncing the reset. S26, the TCP/small-file tables
above and the unchanged-code control below therefore include reset interference
in their ext4 timings; they do not isolate transfer cost. Both harnesses now
preallocate once and finish an in-place `pwrite` plus `fdatasync` before each
window. The production owners, timed operations and byte/gap/guard checks are
unchanged. Buffered reset was chosen to keep verification simple while moving
its writeback and allocation work outside the measured interval.

[Paired trace run 42536940169](https://github.com/OrbitInfra/OrbiTE/actions/runs/42536940169)
compares old harness `521861db` with fixed harness `19ec2009`; both have the
production sources of `4f6bd35c`, against frozen TENT `e1ea5d31`. Each phase
runs twenty implementation/filesystem/direction/shape cases, with 1,000
byte-checked samples and three warmups each. Each old/fixed pair shares one
four-vCPU Ubuntu 24.04 / Linux 7.0.0-1017-azure VM: stable (Rust 1.99.1) uses
EPYC 9V74, and Rust 1.85 uses EPYC 9V45. These are different hosts; do not
pool their absolute times.

`benches/run.py --tail-profile-only` records monotonic sample windows and
system-wide io_uring, block-I/O, thread-creation, `fdatasync` and scheduler
tracepoints. The collector writes to tmpfs and copies evidence to ext4 only
after each benchmark exits. Raw `perf` data, event formats, commands and
windows remain in `cpu-bench-*` artifacts. Run
`python3 benches/analyze_io_tail.py "$OUT/orbite-profiles/tail"` for per-sample
submit/first-CQE/last-CQE/return, worker/caller wake and block timelines.

The analyzer pairs device/flags/sector/length receipts across the **whole
capture**, including writes issued before a window that finish inside it.
It counts same-device traffic from other processes too. Direct candidates
must match the benchmark process, requested operation and synchronous block
flags after SQE submission, and their total bytes must equal the payload.
Missing ring/CQE or scheduler receipts and mismatched direct bytes fail the
analysis. Block events have no SQE identity: this is correlation, not an
invented per-SQE receipt. Extra traffic is reported without assuming that all
of it belongs to the benchmark. Prefix-flush and FUA flags are counted
separately; the analyzer has negative controls for both and for carry-in writes.

Each row below covers 8,000 ext4 windows per phase (4,000 READ and 4,000 WRITE).
Counts are **old → fixed**. Extra writes exclude the measured direct payload;
carry-in means the write was already pending at window start.

| Toolchain / implementation | Extra writes during READ | Extra writes during WRITE | Carry-in writes, both directions | Flushes / FUA, both phases |
|---|---:|---:|---:|---:|
| Stable OrbiTE | 6,001 → 1 | 6,001 → 1 | 12,000 → 1 | 0 / 0 |
| Stable TENT | 6,001 → 1 | 6,000 → 0 | 12,000 → 0 | 0 / 0 |
| Rust 1.85 OrbiTE | 6,000 → 0 | 6,001 → 0 | 12,000 → 0 | 0 / 0 |
| Rust 1.85 TENT | 6,000 → 2 | 5,999 → 0 | 11,999 → 0 | 0 / 0 |

The old reset overlaps 31,999 of 32,000 measured ext4 windows. After the fix,
**no reset writeback overlaps a window**. The five remaining writes are
4 KiB each from other processes (three stable, two MSRV); one stable write
straddles window start. There are three discard requests per fixed phase,
zero flush/FUA requests and zero unmatched overlapping issues. This confirms
a measurement artifact in the old harness, but does not explain every tail.

The following values are **diagnostic traces**, not benchmark p99s: each is
the median of the slowest ten samples in its 1,000-sample case. CQE and block
completion mean kernel events, not physical hardware timestamps. Runnable
time covers observed wake/preemption intervals after the issuer first runs;
initial thread creation/scheduling is in the before-first-SQE timeline.

| 64 × 4 KiB gapped | Batch ms, old → fixed | Fixed longest block wait ms | Fixed issuer blocked / runnable µs | Fixed last block → last CQE µs |
|---|---:|---:|---:|---:|
| Stable OrbiTE READ | 30.30 → 29.11 | 28.41 | 28,185 / 61 | 17.2 |
| Stable OrbiTE WRITE | 75.46 → 23.79 | 23.06 | 23,160 / 9 | 20.6 |
| Stable TENT READ | 101.27 → 6.60 | 6.00 | 5,523 / 131 | 6.3 |
| Stable TENT WRITE | 4.98 → 4.32 | 3.59 | 3,538 / 66 | 15.7 |
| Rust 1.85 OrbiTE READ | 30.01 → 32.30 | 31.72 | 31,433 / 24 | 4.9 |
| Rust 1.85 OrbiTE WRITE | 5.97 → 31.81 | 31.31 | 31,252 / 8 | 10.0 |
| Rust 1.85 TENT READ | 26.22 → 24.98 | 24.13 | 23,257 / 368 | 6.1 |
| Rust 1.85 TENT WRITE | 4.67 → 4.19 | 3.16 | 3,411 / 61 | 15.1 |

In particular, the fixed MSRV OrbiTE gapped WRITE case has no extra writes,
flushes, FUA or discards in any of its 1,000 windows, yet its long tails contain
31 ms block waits. Thus the clean reset removes a proven confounder; it does
**not** establish that the whole S26 tail or the implementation gap was a
harness artifact. The residual direct-I/O tail remains open. No production
tuning or physical-SSD claim is justified by these traces.

The proposed engine explanations can be narrowed:

- Every measured gapped batch uses **one ring, 64 SQEs and one submitting
  enter** in both implementations. In the fixed slow-tail
  samples, OrbiTE uses a median 2–4 enters including zero-submission waits;
  TENT uses one submitting enter and polls the mapped CQ. These waits never resubmit an SQE.
- All 64-block CQEs are published on the submitting batch worker. There is
  no caller-to-worker ring handoff in this case. Worker drain has **zero
  nanosleeps**; the benchmark caller still uses its documented status backoff.
- At that revision aligned gaps required no bounce allocation. Current direct
  READs isolate the destination until the CQE to preserve bytes beyond EOF.
  The [ext4 READ study](#eight-contiguous-direct-reads) covers that change.
  Aligned WRITEs and buffered READs still check one ring and intact guards
  with every bounce allocation denied.
- The traced direct-I/O workload creates **one OrbiTE batch worker** per multi-request submission; TENT
  creates none. Before-first-SQE time in OrbiTE's slow 64-block samples is
  105–148 µs in the fixed paired traces, much shorter than the block wait. Removing thread creation
  cannot by itself explain or eliminate the measured millisecond tail.

These historical traces also isolated the small tmpfs WRITE handoff: the
median sample created a batch worker after caller submission, versus zero
for READ and TENT. Diagnostic WRITE medians were 137/118 µs (stable/MSRV),
versus TENT's 81/59 µs; they are not untraced S26 results. The
[small-file paired pass](#small-file-paired-results) now measures a bounded
caller CQ wait for the existing eligible request. Expiry still hands the
same owner to the worker and is never a release receipt. The multi-request
direct-I/O owner and the historical tail diagnosis remain unchanged.

### Direct-I/O issue paths and verification load

The [block-layer comparison](https://github.com/OrbitInfra/OrbiTE/actions/runs/42562245093)
uses production `82cf6cef` with diagnostic harness `2fba491d`. The
[counterfactual run](https://github.com/OrbitInfra/OrbiTE/actions/runs/42563852934)
uses `dca107d`, with the **same production tree**, against frozen TENT
`e1ea5d31`. Each diagnostic case has three warmups and 1,000 byte-checked
samples. The latter run uses four-vCPU Ubuntu 24.04 / Linux
7.0.0-1017-azure VMs: stable Rust 1.99.1 on EPYC 9V45, Rust 1.85 on
EPYC 7763. The initial MSRV trace and stable counterfactual expose
`MSFT NVMe Accelerator v1.0`, with a 256 KiB request limit; the counterfactual
MSRV job exposes `Msft Virtual Disk` (`sda`), with a 2 MiB limit. Both report
512-byte logical / 4 KiB physical sectors and the `none` scheduler. Each
within-job engine comparison shares its device; the jobs are distinct virtual
disks, not installed NVMe qualification.

Both implementations open the payload descriptor with `O_RDWR|O_DIRECT`,
without `O_DSYNC` or `O_SYNC`; OrbiTE also uses `O_CLOEXEC`. The separate
OrbiTE buffered descriptor's `O_NONBLOCK` is absent from the direct descriptor.
`strace` records opens and subsequent `fcntl` calls. Plain ring setup, SQEs,
file offsets and device requests agree. For each gapped 64-block WRITE:

| Boundary | OrbiTE | Frozen TENT |
| --- | --- | --- |
| SQE submission | Existing batch worker, one enter | Caller, one enter |
| Ring / SQEs | One / 64 `WRITEV` | One / 64 `WRITEV` |
| File offsets / lengths | 4 KiB + request index × 8 KiB / 4 KiB | Same |
| Kernel issue | 64 NOWAIT direct attempts, then 64 io-wq direct issues | Same |
| Bios / alignment | 64 × 4 KiB, 4 KiB-aligned sectors, stride 16 sectors | Same |
| Merges / unplug width | Zero merges; 64 unplugs of one request | Same |
| Median of per-batch maximum driver-issue depth, initial MSRV run | 9 | 10 |
| Cached folios invalidated after reset | 49: orders 0–2 | 14: orders 0–5 |
| Untimed verification reads, per sample | 32 × 4 KiB plus one large read | 5 × 4 KiB plus five larger reads |

The first repeatable divergence is **verification before the next sample**.
Rust reads the whole file; the earlier C++ harness used an `istreambuf_iterator`
with small buffered reads. Their readahead and cached-folio layouts differ even
though both reset in place and finish `fdatasync`. In the initial MSRV trace,
median between-window time is 1.72 / 2.81 ms (OrbiTE / TENT). All measured
WRITEs are issued before the long waits: OrbiTE's slowest-1% median last issue
is 0.82 ms, while the longest block wait is 29.68 ms. No delayed submit,
extra SQE, bounce allocation, flush/FUA or ownership wait explains that gap.
The trace's queue depth is this batch's outstanding driver requests, not
hardware-wide queue depth. Physical disk service is not visible to the guest.

Two controls leave production and the timed operation unchanged. A bulk-read
C++ verifier preserves every byte/guard check and reproduces the long tail in
TENT. An additional **untimed 10 ms pause after reset** reduces offered load.
Numbers below are medians of the slowest ten traced samples, in milliseconds;
they are **not** untraced benchmark p99s. Stable / Rust 1.85 stay separate.

| 64 × 4 KiB WRITE | Original verifier | TENT bulk verifier only | Untimed pause only |
| --- | ---: | ---: | ---: |
| OrbiTE gapped | 30.78 / 34.16 | — | 1.54 / 2.58 |
| TENT gapped | 3.15 / 3.35 | 29.83 / 28.79 | 2.83 / 3.29 |
| OrbiTE contiguous | 26.58 / 19.71 | — | 17.41 / 3.00 |
| TENT contiguous | 25.61 / 12.89 | 25.01 / 17.48 | 1.88 / 2.77 |

A [reversed-order run](https://github.com/OrbitInfra/OrbiTE/actions/runs/42567993578)
on source `fc3c2a5` runs bulk verification first and the legacy verifier later.
TENT gapped tail medians are 27.03 / 30.12 ms with bulk and 3.28 / 3.39 ms
with legacy verification. The effect persists in both orders.

The original TENT bulk-control longest block waits are 28.45 / 27.48 ms. Thus an
unchanged TENT owner exhibits the purported OrbiTE-specific gapped tail when
only verification changes. Lower offered load reduces it without replacing
OrbiTE's worker, ring or drain; it does **not** eliminate every outlier (notably
stable contiguous OrbiTE). Periodic stalls after driver issue are consistent
with virtual-disk rate limiting, but the host's quota/controller is not exposed:
that mechanism is an inference, not a measured throttle receipt. The evidence
supports a verification/load-dependent comparison and a limit below guest
issue, not a production ownership fix or a general storage speed claim.

Both normal harnesses now verify files with a bulk read. The legacy C++
verifier and pacing are diagnostic controls only; `benches/run.py` strips all
inherited `ORBITE_BENCH_*` settings and records explicit diagnostic overrides.
No pacing is added to the normal S26 sweep. The
[complete paired report](https://github.com/OrbitInfra/OrbiTE/pull/186) records
every row with the original trials, samples, warmup and byte checks. Use
`benches/compare.py BEFORE AFTER --changed-profile none --verification-comparison`:
the explicit mode permits the C++ verification transition only, keeps reset
and Rust-workload checks, and labels affected TENT file rows `V`. TENT CPU/TCP
rows remain noise references; changed TENT file rows do not. The clean-reset
numbers below remain evidence for the **earlier verification regime**, not
proof that OrbiTE issues intrinsically slower direct WRITEs. Current virtual-disk
tails remain a P11 limit for the installed NVMe batch. The bounded small-WRITE
wait is measured separately above; a missed window still needs worker drain.

### Reset-only paired comparison

The [untraced paired run](https://github.com/OrbitInfra/OrbiTE/actions/runs/42536943466)
passes all **480 old and 480 fixed rows on each toolchain**, `521861db` →
`19ec2009`. Each pair uses one four-vCPU EPYC 7763 VM, Ubuntu 24.04 / Linux
7.0.0-1017-azure, GCC 13.3.0, and stable Rust 1.99.1 or Rust 1.85.0. The
original S26 three trials, 100 samples, three warmups, two configured workers,
sizes/shapes and byte checks are unchanged. Production sources are identical.
The complete [stable table](https://github.com/OrbitInfra/OrbiTE/pull/184#issuecomment-6730046861)
and [1.85 table](https://github.com/OrbitInfra/OrbiTE/pull/184#issuecomment-6730078580)
include every ext4 row for both implementations, CPU/throughput and every
numerical regression. Raw results remain outside Git.

These are medians of the three untraced trial p99s, in milliseconds:

| Toolchain / 64 × 4 KiB shape | OrbiTE p99 old → fixed | TENT p99 old → fixed |
|---|---:|---:|
| Stable gapped READ | 15.60 → 10.41 | 2.04 → 1.74 |
| Stable gapped WRITE | 8.08 → 32.98 | 1.67 → 1.82 |
| Stable contiguous WRITE | 1.09 → 14.09 | 1.56 → 3.97 |
| Rust 1.85 gapped READ | 17.17 → 10.63 | 2.44 → 1.59 |
| Rust 1.85 gapped WRITE | 10.83 → 25.98 | 2.67 → 1.76 |
| Rust 1.85 contiguous WRITE | 1.02 → 18.00 | 1.39 → 6.18 |

The reset artifact is removed, but the tail and implementation gap do **not**
largely disappear. WRITE tails worsen, with nonoverlapping OrbiTE trial ranges
in both shapes/toolchains; contiguous TENT WRITE worsens too. These regressions
remain open. Small tmpfs WRITE p50 is 194→203 / 194→195 µs (stable/MSRV),
versus TENT after 81/79 µs. HP-TCP 1 MiB READ p50 moves 605→548 / 548→588 µs;
the MSRV increase has nonoverlapping ranges. No watch item is declared fixed.

Stable/MSRV have 110/114 of 160 cases with a worse statistic; 71 in each pair
have all four trial ranges overlapping. Non-file rows do not execute reset
and remain run/build-placement controls; their clock/cache/VM contributions
are unisolated. File preparation intentionally changes allocation/cache state
and I/O timing in **both** harnesses, so TENT file rows are not pure noise
controls. Neither those controls nor range overlap explain away the file gap;
range overlap is not a significance test. This is a measurement correction,
not an engine speedup.

Reproduce with `benches/compare.py BEFORE AFTER --changed-profile none --reset-comparison`.
The flag permits exactly the documented reset transition and changed harness
hashes; all other workload/host/CPU checks remain. Instantaneous clock readings
are reported separately, not normalized.

### Unchanged-code paired control

The [paired control run](https://github.com/OrbitInfra/OrbiTE/actions/runs/42522826931)
uses the **old harness**, `7e40c7b` → `23f6911`, on each toolchain: identical production sources and
workload hashes, with the ordinary S26 three trials, 100 samples and byte checks.
All **480 rows per side passed**. The complete
[stable](https://github.com/OrbitInfra/OrbiTE/pull/184#issuecomment-6728129888) and
[1.85](https://github.com/OrbitInfra/OrbiTE/pull/184#issuecomment-6728131269)
tables include every slower row. Stable uses four EPYC 7763 logical CPUs;
1.85 uses four Xeon 6973P-C logical CPUs. Each pair shares one VM with Ubuntu
24.04 / Linux 7.0.0-1017-azure. In the 1.85 pair, only the instantaneous
`lscpu` scaling reading differs (116% → 154%); model, topology, flags and
frequency limits match. Frequency was not locked or used to normalize times.
Reproduce them with `benches/compare.py BEFORE AFTER --changed-profile none`.

Stable medians of the three trial statistics; all ext4 rows below use 4 KiB blocks:

| Workload | OrbiTE p50 µs before → after | OrbiTE p99 µs before → after | TENT p99 µs before → after |
|---|---:|---:|---:|
| ext4, 64 gapped READ | 2,220.5 → 1,652.3 | 5,424.5 → 6,536.6 | 4,335.6 → 5,419.0 |
| ext4, 64 gapped WRITE | 853.6 → 842.5 | 2,517.7 → 4,486.7 | 1,762.1 → 2,851.6 |
| ext4, 8 gapped READ | — | 1,582.4 → 777.8 | 1,077.4 → 919.8 |
| ext4, 8 gapped WRITE | — | 643.4 → 681.9 | 891.2 → 1,443.6 |
| ext4, 8 contiguous READ | — | 833.8 → 803.1 | 986.0 → 819.1 |
| ext4, 8 contiguous WRITE | — | 757.7 → 673.7 | 593.9 → 755.4 |

Rust 1.85's gapped 64-block READ/WRITE p99 changes 16.36→15.28 / 4.54→4.41 ms;
TENT changes 1.35→3.40 / 2.15→2.28 ms. Stable tmpfs READ/WRITE p50 changes
52.5→48.2 / 198.9→190.8 µs, versus TENT after 83.9/83.5 µs: the WRITE
handoff gap remains. HP-TCP 1 MiB READ p50 changes 543.6→541.0 µs on stable
and 610.3→583.8 µs on 1.85; this does not establish a fix to its watch item.

All payloads are unchanged. Stable has 141/160 cases with at least one worse
statistic, 105 with overlapping ranges for all four statistics; 1.85 has
104/160 and 84. Not all changes track TENT: stable single-block ext4 READ p99
rises 157% while TENT falls 5%, with nonoverlapping OrbiTE trial ranges.
Conversely, contiguous 64-block READ p50 rises 45% while TENT rises 57%.
The unchanged 1.85 SHM contiguous 64-block WRITE p50 rises 56%, despite no file
I/O. These are run/build-placement variations, not optimization results;
their clock, cache and VM contributions are not isolated. Every regression
remains flagged in the tables. Range overlap is descriptive, not a significance
test. Raw results remain outside Git; no physical-SSD claim follows.

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
socket failures from injected completion faults. io_uring uses real files/rings.
A missing required kernel facility fails, never silently skips.
SHM's synchronous memcpy has no native cancel or partial-post queue; queued
cancellation belongs to the common batch owner. Local files have no peer epoch:
path replacement is tested against the admitted inode. Those dispositions are
explicit, not counted as passes.

```bash
cargo test --locked --features io-uring --lib --no-run --message-format=json > "$OUT/build.json"
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
