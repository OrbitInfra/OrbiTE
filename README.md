<div align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="docs/_static/orbite-logo-dark.svg">
    <img src="docs/_static/orbite-logo.svg" width="60%" alt="OrbiTE">
  </picture>
  <h2>The KV data plane for LLM inference on NVIDIA</h2>
  <a href="https://orbitinfra.github.io/OrbiTE/"><strong>Documentation</strong></a>
  | <a href="https://orbitinfra.github.io/OrbiTE/getting-started.html"><strong>Quick Start</strong></a>
  | <a href="https://orbitinfra.github.io/OrbiTE/architecture.html"><strong>Architecture</strong></a>
  | <a href="docs/tent-parity.md"><strong>TENT Reference</strong></a>
  <br><br>

  [![CI](https://github.com/OrbitInfra/OrbiTE/actions/workflows/ci.yml/badge.svg)](https://github.com/OrbitInfra/OrbiTE/actions/workflows/ci.yml)
  [![Crates.io](https://img.shields.io/crates/v/orbite.svg)](https://crates.io/crates/orbite)
  [![Rust](https://img.shields.io/badge/rust-1.85%2B-orange.svg)](Cargo.toml)
  [![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)
</div>

## Overview

OrbiTE is an independent transfer engine whose design follows Mooncake TENT; it is not wire-, API- or configuration-compatible with TENT.

OrbiTE is the KV data plane for LLM inference on NVIDIA. It moves KV between
GPUs, DRAM, local SSD and peer hosts with two guarantees: memory, registrations
and staging are never released before the hardware provably stops accessing
them, and transfers are scheduled by inference intent (decode-critical first,
prefetch and write-back yield).

[OrbitKV](https://github.com/OrbitInfra/OrbitKV) is the first consumer, for cache
tiering and cross-node reuse. Prefill→decode (P/D) KV transfer is the second
target use case, using the same transfer and lifetime primitive. The
[post-0.1.0 plan](docs/completion-plan.md#release-ladder) starts with a NIXL
backend design, after the GDR and NVLink installed profiles qualify.

The design follows Mooncake TENT's engine, transport, context, buffer, endpoint
and worker boundaries in Rust. Intent scheduling uses configured admission and
QoS policies; transport-specific priority remains as documented in
[transport selection](docs/transport-selection.md). Current support and
hardware evidence are listed in [capabilities](docs/capabilities.md).

Non-goals:

- Cluster discovery and metadata, which belong to the consumer.
- Collectives.
- Non-NVIDIA accelerators.
- TENT wire interoperability.
- General-purpose RPC.

<div align="center">
  <img src="docs/_static/architecture.svg" width="95%" alt="OrbiTE network transports and the implemented DRAM and GPU paths to local SSD">
</div>

## Updates

- **Nov 8, 2026**: TCP reuses bounded connections keyed by peer epoch and waits
  on socket readiness. The CPU performance pass records matched results and
  remaining regressions. [Benchmarks](docs/benchmarks.md#paired-results-and-regressions)
- **Nov 8, 2026**: OrbiTE owns its strict JSON/`ORBITE_CONF` configuration and
  Rust-shaped Python API. TENT API, configuration and naming compatibility are
  removed; frozen TENT remains the design reference and benchmark baseline.
- **Nov 8, 2026**: The beta.1 Rust and Python API freeze decisions are settled.
  Publishing remains the maintainer's action. [Release notes](docs/releases.md)
- **Nov 7, 2026**: Byte-checked CPU size/batch sweeps compare against frozen
  TENT, with an executable CPU fault matrix and explicit hardware gaps.
  [Method and limits](docs/benchmarks.md)
- **Nov 7, 2026**: The opt-in libfabric host path moves real bytes through the
  software TCP provider in Linux CI. EFA remains unqualified. [Libfabric](docs/fabric.md)
- **Nov 3, 2026**: Local file segments support DRAM transfers through io_uring,
  CUDA staging through pinned DRAM, and GDS through cuFile. The DRAM path is
  qualified on ext4 and tmpfs in CI. [Local storage](docs/storage.md)
- **Nov 3, 2026**: Ordered transport policies pick NVLink, RDMA or TCP
  per request.
- **Nov 2, 2026**: FIFO/EDF admission windows, compiled QoS contracts and
  receiver credit windows.
- **Nov 1, 2026**: Bandwidth-aware multirail selection from CUDA/NUMA topology
  and configured NIC preference tiers; CUDA 13 native batch copies for NVLink groups.
- **Oct 28, 2026**: Bounded GPUDirect RDMA, CUDA IPC and remote CUDA staging
  profiles pass installed H20/RoCE tests.
- **Oct 27, 2026**: The bounded host RDMA RoCE profile is independently accepted.

## Highlights

- **Local SSD transfers** move registered DRAM and CUDA buffers through io_uring or cuFile. [Storage guide](docs/storage.md)
- **Owned memory, explicit completion.** Registrations keep their real
  allocations, and nothing is released until the backend proves it can no
  longer touch the memory, including after failed native cleanup.
- **Multirail, topology-aware RDMA.** Persistent MRs, reusable QP arrays and
  shared CQ workers spread slices across NICs by NUMA/PCIe distance and
  measured bandwidth, with periodic route recovery.
- **NVIDIA data paths.** GPUDirect RDMA, CUDA IPC/NVLink with native batch
  copies, and pinned staging for TCP and RDMA, all on the CUDA Driver API.
- **Bounded admission and QoS.** High/Medium/Low priority, FIFO/EDF dispatch
  windows, per-tenant QoS budgets and receiver credits limit what each peer
  can hold in flight.
- **Policy-based transport selection.** Ordered, first-match rules choose an
  eligible NVLink, RDMA or TCP path once per request and honor explicit
  [hints](docs/transport-selection.md).

## Supported paths

| Transport | Memory | Status |
| --- | --- | --- |
| TCP | DRAM; CUDA through staging | Implemented |
| RDMA | DRAM; CUDA through GPUDirect or staging | Implemented; bounded RoCE profiles accepted |
| NVLink / CUDA IPC | CUDA and pinned DRAM on one host | Implemented; bounded H20 profiles accepted |
| io_uring, GDS | DRAM/CUDA ↔ local SSD | Implemented (P7) |
| [HP-TCP](docs/hp-tcp.md) | DRAM over persistent IPv4 lanes | Qualified on Linux loopback. Physical rails unqualified |
| [SHM](docs/shm.md) | Same-host DRAM over POSIX SHM | Qualified on Linux CI. Physical CXL unqualified |
| [MNNVL / EGM](docs/cuda-ipc.md#multi-node-nvlink-and-egm) | GPU and host-NUMA memory over an NVLink fabric | Implemented. Installed profile unqualified |
| libfabric | Host DRAM RMA, opt-in software TCP/EFA profiles | Implemented; EFA installed profile unqualified |
| MPComm | Optional network provider | Deferred: no consumer; provider unavailable |

Rust is the primary API; the [Python API](docs/python.md) mirrors OrbiTE's owned P2P operations. See the [wire and consumer boundary](docs/tent-parity.md#wire-and-consumer-boundaries).
[Capabilities](docs/capabilities.md) lists the exact limits of each path.

## Quick Start

On Linux with Rust 1.85 or later:

```sh
git clone https://github.com/OrbitInfra/OrbiTE.git
cd OrbiTE
cargo run --locked --example tcp_roundtrip
```

Expected output:

```text
17 bytes: hello from OrbiTE
```

The CPU/TCP example needs no CUDA or RDMA SDK. Enable the `rdma` and `cuda`
features with the [build guide](docs/build.md), then continue with the
[Rust API guide](docs/rust-api.md) and [RDMA/CUDA guide](docs/rdma-cuda.md).

## Status

The source is preparing [0.1.0-beta.1](docs/releases.md). The bounded host RDMA, GPUDirect RDMA, CUDA IPC and
staging profiles above passed installed tests on H20/RoCE hosts and
independent review; newer source has CPU controls and awaits its hardware pass.
[Qualification](docs/qualification.md) records the evidence, and the
[completion plan](docs/completion-plan.md) is the work queue. `orbite 0.1.0-alpha.1`
is published on crates.io for early development; its API is unstable.

## Documentation

| Topic | Guides |
| --- | --- |
| Getting started | [Quick Start](docs/getting-started.md), [Build](docs/build.md), [Capabilities](docs/capabilities.md) |
| Using the engine | [TCP and memory ownership](docs/cpu-tcp.md), [Async batches](docs/async-batches.md), [RDMA and CUDA](docs/rdma-cuda.md), [CUDA IPC](docs/cuda-ipc.md), [Staging](docs/staging.md) |
| Design and API | [Architecture](docs/architecture.md), [Rust API](docs/rust-api.md), [TENT design reference](docs/tent-parity.md) |
| Development | [Contributing](docs/contributing.md), [Qualification](docs/qualification.md), [Completion plan](docs/completion-plan.md) |
| Operations | [Troubleshooting](docs/troubleshooting.md), [Installed host validation](docs/installed-host.md), [Native cleanup](docs/native-cleanup.md) |

## Contributing

Start with the [contributor guide](docs/contributing.md) and the
[source reading order](docs/architecture.md#source-reading-order). Coding agents
follow [AGENTS.md](AGENTS.md).

## License

OrbiTE is an independent [OrbitInfra](https://github.com/OrbitInfra) project,
licensed under [Apache-2.0](LICENSE). [Mooncake TENT](https://github.com/OrbitInfra/Mooncake/tree/e1ea5d31bb5e46ff2df9f90e7c2fbaf52fcdee8c/mooncake-transfer-engine/tent)
is its frozen capability and architecture reference.
