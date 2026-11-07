# Release notes

## 0.1.0-beta.1 — preparation

This is a release candidate for maintainer review, not a publication record.
The Rust and Python APIs are the freeze candidate; the
[Rust review findings](rust-api.md#freeze-review-findings) and
[Python surface review](python.md#surface-and-defaults) identify decisions that
remain with the maintainer. Publishing to crates.io and PyPI is the
maintainer's action. The last published Rust milestone is `0.1.0-alpha.1`
from `7cc48490`; Python artifacts use the equivalent version `0.1.0b1`.

### Changes since 0.1.0-alpha.1

- Rust is the primary interface. Python lives in the separate
  `orbite-python` packaging crate and exposes the frozen `mooncake.tent`
  P2P subset as `import orbite as tent`. Installed wheels cover CPython
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
  its TENT-shaped reversible handle semantics.
- The S23 TENT wire interoperability profile was removed. Both peers run the
  same OrbiTE build; no autodetection or downgrade is available. TENT-format
  configuration, metric names, Python API correspondence and the frozen
  v0.3.14 CPU oracle remain. See the
  [wire and consumer boundary](tent-parity.md#wire-and-consumer-boundaries).
- Opt-in libfabric is implemented with real CPU bytes on the software TCP
  provider. MPComm is deferred: no consumer and no available provider;
  its recognized Python transport constant returns unavailable.
- S26 adds verified size/batch sweeps against frozen TENT and 24 executable
  CPU fault controls, including real requester kills during partial TCP and
  HP-TCP WRITEs. Raw measurements stay outside Git. No tuning is included.
- The [product positioning](https://github.com/OrbitInfra/OrbiTE#overview)
  centers OrbitKV cache tiering and cross-node KV reuse. P/D is a later NIXL
  integration design, after installed GDR/NVLink qualification; no custom
  connector or NIXL dependency is added.

This preparation unifies the Rust package versions, checks wheel and sdist
metadata, hides five accidental Python helper imports, and corrects stale
documentation. It does not change transfer behavior. TCP, configuration and
the Python CPU/TCP wheel profile are labeled **Qualified on Linux CI** only
against the [named jobs and tests](qualification.md#linux-ci-release-profiles).
HP-TCP loopback, same-host SHM and ext4/tmpfs io_uring retain their bounded
Linux CI qualification.

### Known limitations

Every **current GPU, RDMA, GDS, MNNVL/EGM, EFA and CXL profile is unqualified**.
This includes GPUDirect, staging, caller-stream local copies, NVLink and Python
CUDA exporters. Older accepted H20/RoCE artifacts do not qualify this release.
Physical two-host TCP and physical multi-rail remain unqualified. CPU shims and
software libfabric TCP-provider tests are not device evidence. Required installed
gates remain in the [verification queue](qualification.md#verification-queue).

S26 recorded these CPU slow paths on its four-vCPU Linux runner against frozen
TENT. They are known P11 issues, not generalized performance claims or release
speed guarantees. No tuning was attempted in the benchmark preparation.

| Workload | OrbiTE relative to TENT / measured latency |
| --- | --- |
| Scattered 64 × 4 KiB TCP READ / WRITE | p50 **1.36× / 1.53×** TENT |
| 4 KiB buffered tmpfs READ / WRITE | p50 **1.88× / 2.34×** TENT |
| Gapped ext4 READ / WRITE | p99 **15.7 / 8.8 ms**, versus **1.8 / 1.7 ms** for TENT |

The [original measurement table](qualification.md#cpu-benchmark-evidence)
pins revisions, host, workload and sample counts. Per-batch workers and
per-owner TCP connections are possible contributors; the ext4 tail cause is
unresolved. p99 uses only 100 samples per trial, and files were warmed without
fsync. These results say nothing about RNIC, GPU or physical SSD performance.

Timeout, cancellation and terminal status remain distinct from a physical
release receipt. Uncertain cleanup retains owners; uncertain WRITEs are never
replayed. Before `0.1.0`, peers must run the same build. The explicit
compatibility policy and hardware gates start at the
[stable release boundary](completion-plan.md#release-ladder).
