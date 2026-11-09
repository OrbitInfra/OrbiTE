# Release notes

## 0.1.0-beta.1 — preparation

This is a release candidate for maintainer review, not a publication record.
The Rust and Python APIs are the freeze candidate; the
[Rust freeze decisions](rust-api.md#freeze-decisions) and
[Python surface review](python.md#surface-and-defaults) record the maintainer's
decisions and the boundaries excluded from the freeze. Publishing to crates.io
and PyPI is the
maintainer's action. The last published Rust milestone is `0.1.0-alpha.1`
from `7cc48490`. The beta.1 version is spelled `0.1.0b1` on PyPI.

### Changes since 0.1.0-alpha.1

- Rust is the primary interface. Python lives in the separate
  `orbite-python` packaging crate and mirrors the Rust API with PEP 8 names.
  Installed wheels cover CPython
  3.10–3.14 on Linux x86_64/aarch64; blocking calls release the GIL.
  Buffer exporters and CUDA Array Interface/DLPack owners stay retained
  through native drain, including cancellation and uncertain cleanup.
- The C ABI (`libtent`, the `tent_*` exports), native/classic C++ adapters,
  device plugin ABI and diagnostics C interface were removed. No classic
  `mooncake.engine` shim is shipped. The offline CLI is now
  `orbite diagnostics version [--json]`, avoiding Mooncake's `tent` command.
  Rust diagnostics retain text/JSON output and exit codes.
- Discovery is P2P-only. Consumers own membership, KV indexes and metadata
  stores, publish endpoint/epoch authority, and retire departed peers.
  Non-P2P configuration is explicitly rejected. Rust per-peer retirement
  fences new work, drains existing owners and reclaims unreferenced identities;
  failed native cleanup stays retained and charged. Python segment close keeps
  reversible handle semantics.
- The S23 TENT wire interoperability profile was removed. Both peers run the
  same OrbiTE build; no autodetection or downgrade is available. The frozen v0.3.14 CPU oracle remains as a behavior baseline. See the
  [wire and consumer boundary](tent-parity.md#wire-and-consumer-boundaries).
- Opt-in libfabric is implemented with real CPU bytes on the software TCP
  provider. MPComm is deferred: no consumer and no available provider;
  it has no public Python transport identity.
- S26 adds verified size/batch sweeps against frozen TENT and 24 executable
  CPU fault controls, including real requester kills during partial TCP and
  HP-TCP WRITEs. Raw measurements stay outside Git. No tuning is included.
- The [product positioning](https://github.com/OrbitInfra/OrbiTE#overview)
  centers OrbitKV cache tiering and cross-node KV reuse. P/D is a later NIXL
  integration design, after installed GDR/NVLink qualification; no custom
  connector or NIXL dependency is added.

This preparation unifies package versions and validates wheel/sdist metadata,
including under optimized Python. Rust public field records and enums are
non-exhaustive except binary `TransferOp`; construct configs through defaults
and field assignment. `NVLinkStats` is renamed to `NvlinkStats`, `MemoryBacking`
is the real type name, and `TopologyMemoryKind` names topology entries without reference-specific naming. Binding-only
helpers and the standalone adaptive core are hidden and outside the freeze;
the Python crate pins the exact core version. Five accidental Python helper
imports are private. Python's additive helpers and reversible segment close
are ratified. Missing documentation must close before `0.1.0`. Transfer behavior
is unchanged. TCP, configuration and
the Python CPU/TCP wheel profile are labeled **Qualified on Linux CI** only
against the [named jobs and tests](qualification.md#linux-ci-release-profiles).
HP-TCP loopback, same-host SHM and ext4/tmpfs io_uring retain their bounded
Linux CI qualification.

### Independent interfaces

TENT remains the frozen design reference, CPU oracle and performance baseline.
Wire, API, configuration and naming compatibility are out of scope. The
compatibility loaders, MC_/NCCL/MOONCAKE aliases and S16 compatibility inventories
are removed. Rust exposes `EngineConfig::from_json/from_file/from_env`;
only `ORBITE_CONF` configures the environment loader. Python now mirrors Rust
owners with PEP 8 methods and an `orbite.Error` hierarchy. Exporter retention,
GIL release, cancellation and physical drain guarantees are unchanged.

Metrics use the `orbite_` prefix. Public TENT transport-ID conversion methods
and excluded Python transport identities are removed; diagnostics reports only
OrbiTE transports. `TopologyMemoryKind` replaces `MemType`, distinct from
registration `MemoryType`. Native comparison jobs have 120-minute headroom;
the 480 byte-checked rows and sample counts are unchanged.

### Known limitations

Every **current GPU, RDMA, GDS, MNNVL/EGM, EFA and CXL profile is unqualified**.
This includes GPUDirect, staging, caller-stream local copies, NVLink and Python
CUDA exporters. Older accepted H20/RoCE artifacts do not qualify this release.
Physical two-host TCP and physical multi-rail remain unqualified. CPU shims and
software libfabric TCP-provider tests are not device evidence. Required installed
gates remain in the [verification queue](qualification.md#verification-queue).

The profile-guided TCP and small-file passes repeat S26 on four-vCPU EPYC 7763
Linux runners. The TCP pair is `f0556448` → `de9fd60b`; the file pair is
`edec7937` → `a52e7b7`. Each pair shares one VM, with full byte checks and
frozen TENT on that runner. The table reports medians of three 100-sample
trials; the two jobs are not pooled. These are scoped measurements, not
release speed guarantees. The reset-only pair (`521861db` → `19ec2009`)
also uses one EPYC 7763 VM per toolchain; it changes both harnesses, with
identical production code.

| Workload | Paired result / remaining limit |
| --- | --- |
| Scattered 64 × 4 KiB TCP READ / WRITE | p50 **8.59 → 4.23 / 9.33 → 6.30 ms**; TENT after **6.18 / 6.07 ms**. WRITE remains 3.7% slower than TENT |
| Single 4 KiB TCP READ | CPU **370 → 190 µs** per batch; TENT after **256 µs** |
| 4 KiB buffered tmpfs READ / WRITE | p50 **156 → 51 / 197 → 193 µs**, or **0.61× / 2.30×** TENT after. CPU **166 → 55 / 250 → 178 µs**; WRITE latency remains a limit |
| Gapped ext4 READ / WRITE | **Clean-reset** p99: stable **10.41 / 32.98 ms** versus TENT **1.74 / 1.82 ms**; Rust 1.85 **10.63 / 25.98 ms** versus **1.59 / 1.76 ms**. [All paired rows](benchmarks.md#reset-only-paired-comparison) retain the regressions. The tail remains open; production owners are unchanged |

The earlier small-file **old-harness** ext4 p99 was 14.9/5.9 ms versus
TENT 2.2/2.6 ms; it included reset writeback and is not isolated transfer cost.
The [original S26 evidence](qualification.md#cpu-benchmark-evidence) keeps its
historical values and labels. The [TCP results](benchmarks.md#paired-results-and-regressions)
and [small-file results](benchmarks.md#small-file-paired-results)
pin revisions, hosts, parameters and every numerical regression. The TCP profile confirmed one
connection per physical request and substantial reply-poll CPU. TCP now reuses
bounded, epoch-scoped streams and uses deadline-bounded socket waits. Dirty
streams retire without replay, and uncertain cleanup retains its owners.
One small host-file operation can now finish on its caller when its CQE is
ready. Pending I/O keeps the same ring and moves to the existing worker;
uncertain handoff retains all owners. No polling contract or ring bound changes.
In the **old-harness** small-file pair, Rust 1.85's 64 KiB ext4 READ/WRITE
p50 regresses 15%/17% while CPU falls
16%/20%; the contribution of changed wake timing versus file/VM variation is
not isolated. Larger file groups keep their worker path. Per-sample traces
rule out extra rings, bounce allocation and drain-loop sleeps for aligned
gaps. They also confirm that the old truncate/rewrite reset contaminated
virtually every ext4 window. Both harnesses now preallocate once and finish
in-place reset writes with `fdatasync` before timing. That removes reset
writeback; it does not eliminate all long direct-I/O waits or establish their
underlying cause. The earlier [unchanged-code control](benchmarks.md#unchanged-code-paired-control)
remains labeled old-harness. Small tmpfs WRITE still usually creates a worker
after the caller sees a pending CQE; its old control-pair p50 is 199→191 µs
versus TENT's 84 µs. The caller-wait candidate is deferred.
There is one exclusive direct-TCP lane per peer, without a concurrent-caller performance
claim. The separate Rust 1.85 pair used a newer EPYC 9V45: scattered TCP
improved, but 64 MiB READ p50/p99 regressed 2.8%/10.7% while CPU fell 4.2%.
Blocking wakeups can trade latency for CPU; the exact contribution is not isolated.
p99 uses only 100 samples per trial. Timed WRITEs still exclude a durability
barrier; reset synchronization is outside the interval. These results say nothing about RNIC, GPU or physical SSD
performance.

Timeout, cancellation and terminal status remain distinct from a physical
release receipt. Uncertain cleanup retains owners; uncertain WRITEs are never
replayed. Before `0.1.0`, peers must run the same build. The explicit
compatibility policy and hardware gates start at the
[stable release boundary](completion-plan.md#release-ladder).
