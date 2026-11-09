<div align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="docs/_static/orbite-logo-dark.svg">
    <img src="docs/_static/orbite-logo.svg" width="60%" alt="OrbiTE">
  </picture>
  <h2>A transfer engine for LLM inference</h2>
  <a href="https://orbitinfra.github.io/OrbiTE/"><strong>Documentation</strong></a>
  | <a href="https://orbitinfra.github.io/OrbiTE/getting-started.html"><strong>Quick Start</strong></a>
  | <a href="https://orbitinfra.github.io/OrbiTE/architecture.html"><strong>Architecture</strong></a>
  <br><br>

  [![CI](https://github.com/OrbitInfra/OrbiTE/actions/workflows/ci.yml/badge.svg)](https://github.com/OrbitInfra/OrbiTE/actions/workflows/ci.yml)
  [![Crates.io](https://img.shields.io/crates/v/orbite.svg)](https://crates.io/crates/orbite)
  [![Rust](https://img.shields.io/badge/rust-1.85%2B-orange.svg)](Cargo.toml)
  [![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)
</div>

OrbiTE moves KV bytes between GPU memory, DRAM, local SSD and peer hosts over
RDMA/GPUDirect, NVLink/MNNVL, TCP, io_uring and GDS, in Rust with a Python API.
Memory, registrations and staging are never released before the hardware provably stops accessing them.
[OrbitKV](https://github.com/OrbitInfra/OrbitKV) is its first user.

<div align="center">
  <img src="docs/_static/architecture.svg" width="95%" alt="OrbiTE network transports and the implemented DRAM and GPU paths to local SSD">
</div>

## Supported paths

| Transport | Memory | Current qualification |
| --- | --- | --- |
| TCP | DRAM; CUDA staging | DRAM: Linux CI; CUDA: unqualified |
| HP-TCP | DRAM | Linux loopback CI |
| SHM | Same-host DRAM | Linux CI; CXL unqualified |
| RDMA | DRAM; CUDA direct or staged | Unqualified |
| NVLink / MNNVL | CUDA; pinned or host-NUMA DRAM | Unqualified |
| io_uring / GDS | DRAM/CUDA ↔ local SSD | DRAM io_uring: Linux CI; CUDA/GDS: unqualified |

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

[Beta.1 source](docs/releases.md); CPU paths are qualified in their stated Linux CI profiles.
Current GPU/RDMA profiles remain unqualified until the hardware batch; see [qualification](docs/qualification.md).

## License

[Apache-2.0](LICENSE). [Contributing](docs/contributing.md).
[Mooncake TENT](https://github.com/OrbitInfra/Mooncake/tree/e1ea5d31bb5e46ff2df9f90e7c2fbaf52fcdee8c/mooncake-transfer-engine/tent) is the design reference.
