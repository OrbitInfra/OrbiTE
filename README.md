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
  [![PyPI](https://img.shields.io/pypi/v/orbite.svg)](https://pypi.org/project/orbite/)
  [![Docs](https://img.shields.io/badge/docs-online-blue.svg)](https://orbitinfra.github.io/OrbiTE/)
</div>

OrbiTE moves KV bytes between GPU memory, DRAM, local SSD and peer hosts over
RDMA/GPUDirect, NVLink/MNNVL, TCP, io_uring and GDS, in Rust with a Python API.
Memory, registrations and staging stay retained until [physical retirement](docs/architecture.md#engine-and-transport-boundary) proves the hardware has stopped accessing them.
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

On Linux (Rust 1.85+, or CPython 3.10–3.14), after beta.1 publication:

```sh
cargo install orbite --version 0.1.0-beta.1 --locked --features io-uring --example orbite-quickstart
orbite-quickstart
# Or Python, in a virtual environment:
python -m pip install orbite==0.1.0b1
python -I -m orbite.quickstart
```

Both programs verify two-process TCP READ/WRITE and DRAM↔file io_uring bytes
and guards, without a GPU. Linux must permit io_uring; an unavailable kernel
is an error. [Source and prerequisites](docs/getting-started.md#installed-quickstart).

## Status

[Beta.1 preparation](docs/releases.md): CPU paths qualified on Linux CI; current GPU/RDMA profiles unqualified on this release ([limits](docs/qualification.md)).
APIs may break between betas; the 0.1.x API and wire freeze begins at 0.1.0.

## License

[Apache-2.0](LICENSE). [Contributing](docs/contributing.md).
[Mooncake TENT](https://github.com/OrbitInfra/Mooncake/tree/e1ea5d31bb5e46ff2df9f90e7c2fbaf52fcdee8c/mooncake-transfer-engine/tent) is the design reference.
