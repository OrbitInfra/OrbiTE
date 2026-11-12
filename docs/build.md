# Build Guide

OrbiTE builds as a Rust library. Provider features are optional and independent.
Rust 1.85 is the minimum supported version. CPU/TCP builds need no native
feature. Enabling `rdma` or `cuda` requires a Linux compilation target.

## CPU and TCP

```sh
cargo build --locked
cargo test --locked
cargo run --locked --example tcp_roundtrip
```

No CUDA or RDMA SDK is required. The runtime does not build or load Mooncake's
C++ engine. Keep `Cargo.lock` when reproducing a source build.

## Host RDMA

On Debian or Ubuntu, install the standard native build prerequisites:

```sh
sudo apt-get install g++ pkg-config clang-18 libclang-18-dev libibverbs-dev
CC=gcc CXX=g++ LIBCLANG_PATH=/usr/lib/llvm-18/lib \
  cargo build --locked --features rdma
```

The RDMA build generates private layouts from the installed `infiniband/verbs.h`
with bindgen. It no longer uses `ibverbs-sys`, whose build script unconditionally
links libibverbs and builds a vendored header tree. No rdma-core library or provider
is bundled. The small C port shim uses the same headers for provider-inline
operations and receives its legacy callback from the runtime table. The build
probes `active_speed_ex`; older headers keep the legacy speed/width path.

`libibverbs.so.1` is loaded only when RDMA is configured or explicitly discovered.
All required exports are checked before opening a device, and the table and DSO
stay loaded for the process lifetime, including uncertain cleanup. Missing
libraries or exports return `Error::Unavailable("libibverbs.so.1: …")`.
`ibv_reg_dmabuf_mr` and `ibv_query_port_speed` remain optional, so older rdma-core
can use peermem and the legacy port-rate path. Header-inline post/poll calls
still dispatch through the provider's context operations.

A combined-feature wheel can import and use TCP or io_uring without rdma-core,
a CUDA driver or cuFile. Enabling their paths requires the host's matching
libraries and providers; wheel repair must not bundle libibverbs. Each RDMA
execution node also needs accessible uverbs character nodes and a valid GID.
A usable context can register memory while its port is paused; actual transfers
require an active, compatible route. CPU/TCP and CUDA-only builds do not compile
the verbs shim.

With a configured RDMA engine and host Direct policy, host registration opens
the usable contexts, including paused ports, and prepares all physical MR copies
before returning a visible range. Native/device or MR-budget errors can therefore
occur during registration. Each local-only view uses one full-view MR per context;
remotely accessible views use two per context (local-only and MW-bind parent).
Published copies are not evicted while their registration is active. Size
`max_cached_mrs` and `max_cached_mr_bytes` for this fan-out. Staged host policy
and CUDA device registration do not eagerly register application backing.

The current CI baseline is Ubuntu 24.04 with Clang 18. A different libclang can
change generated bindings; use the declared baseline when reproducing a
binding-layout failure. See [troubleshooting](troubleshooting.md).

## NVIDIA allocations and host staging

```sh
cargo build --locked --features cuda
cargo build --locked --features rdma,cuda
```

The CUDA feature uses generated `cudarc` Driver API types and selective dynamic
loading. The generated binding baseline is CUDA 13.0; it does not require a
CUDA toolkit or eagerly load every CUDA 13 symbol. Runtime calls need a working
NVIDIA driver. Native NVLink batch dispatch requires reported Driver API version
13000 or later and the exact `cuMemcpyBatchAsync_v2` symbol. Without that optional
capability, groups keep the existing ordered-copy path; required driver symbols
still return an explicit error if unavailable. Device and pinned-host
owners, synchronized copies and host staging are implemented. GPUDirect RDMA,
CUDA IPC and NVLink transfer paths remain separate qualification work.

Compiling these features does not establish hardware execution. See
[qualification](qualification.md) and the [RDMA/CUDA contract](rdma-cuda.md).

## Run source checks

```sh
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
```

With native build prerequisites installed, run the same checks with
`--features rdma` or `--features rdma,cuda`. Native hardware tests are explicitly
ignored by default and require selected devices, original resource locks and
external artifact directories.

## CI tiers

Ordinary PRs and main pushes retain the full Rust feature matrix on stable and
1.85: 28 Clippy/test jobs, including every deterministic ownership control,
real file checks with `ORBITE_REQUIRE_IO_URING=1` and native-library isolation.
Clippy already compiles every target, so a separate `cargo check` is redundant.
Formatting runs once, the feature-independent CPU trace once per toolchain,
and package dry runs cover default and full features. P10 runs every fault
case, including each real peer-kill case once.

Python runs both CPU and combined profiles on CPython 3.13, Linux x86_64 and
aarch64, with both toolchains: four jobs instead of twenty. Each still runs
the installed-wheel lifetime/byte checks and GPU-free quickstart; the sdist
rebuild and clean-container checks remain. No tests are deleted. Documentation
changes confined to `docs/`, README and AGENTS run strict Sphinx/link checks.
Obsolete PR runs are cancelled; release runs are never cancelled automatically.

The **Extended validation** workflow runs Mondays at 02:23 UTC and on manual
dispatch. It retains every 50/200/1,000-iteration stress control, failover
stress, repeated full suites, 50 P10 kill repetitions, all feature-package
checks, all twenty CPython 3.10–3.14 wheel jobs and both 480-row comparisons.
Release preparation always selects this complete tier, including dry runs.
It cannot silently select the reduced Python matrix.

```sh
gh workflow run extended.yml --ref main
# Validate a PR branch, or run only the relevant expensive workflow:
gh workflow run ci.yml --ref <branch> -f extended=true
gh workflow run python.yml --ref <branch> -f extended=true
gh workflow run native-cpu.yml --ref <branch> -f baseline_ref=<base-commit>
```

Run the relevant repetition driver when changing a timing-sensitive owner;
use a matched comparison for performance changes. Ordinary main checks remain
because they validate the merged tree. Infrastructure recovery never retries a
test failure: native-dependency installs have three bounded attempts, repair
interrupted dpkg configuration within the same attempt budget, alternate
mirrors and fail after exhaustion.

## Build the documentation website

The website is generated from this `docs/` tree using Mooncake's Sphinx Book
Theme. See [the contributor instructions](contributing.md#documentation-site)
for the exact build and preview commands.
