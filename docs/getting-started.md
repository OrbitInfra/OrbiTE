# Quick Start

Build the current source on Linux with Rust 1.85 or later. The default CPU/TCP
path needs no CUDA toolkit, RNIC or native provider library. `orbite 0.1.0-alpha.1`
is published on crates.io for early development; its API is unstable. Build
from this checkout to run the examples below.

## Get the source

```sh
git clone https://github.com/OrbitInfra/OrbiTE.git
cd OrbiTE
cargo run --locked --example tcp_roundtrip
```

The example starts two engines, exports a source allocation, discovers its
descriptor using the source owner's epoch, and reads its bytes over loopback.

```text
17 bytes: hello from OrbiTE
```

## Register, discover and transfer

The complete program is [tcp_roundtrip.rs](../examples/tcp_roundtrip.rs).
The central transfer uses registered allocation addresses:

```rust
let request = TransferRequest::new(
    orbite::Transport::Tcp,
    destination.base_address(),
    remote.base_address(),
    exported.len(),
);
let report = requester.submit_and_wait(
    TransferOp::Read,
    &peer,
    &[request],
    Duration::from_secs(1),
)?;
```

`register_memory` gives the engine ownership of host bytes. `open_segment`
requires the authoritative source epoch; a peer at the same address cannot
replace that authority after restart. Requests resolve registered ranges and
keep their actual backing alive through completion.

Check `report.failure` as well as the completed request and byte counts. A
timeout is not proof that native DMA stopped. See the
[Rust API guide](rust-api.md) and [completion contract](cpu-tcp.md).

## Try the other CPU examples

```sh
cargo run --locked --example external_host
cargo run --locked --example notifications
cargo run --locked --example async_batch
```

| Example | What it demonstrates |
| --- | --- |
| `external_host` | Register ranges of an existing allocation while retaining its owner |
| `notifications` | Transfer data and consume the attached completion notification |
| `async_batch` | Submit and seal an owned asynchronous batch, then wait for its result |

The [async guide](async-batches.md) covers cancellation, detached handles and
resource retention. CPU/TCP can cross machines after selecting a routable
`EngineConfig::bind_addr`; the default binds loopback only.

## Enable RDMA or NVIDIA support

Use the [build guide](build.md) for prerequisites and feature flags. Host RDMA
uses libibverbs and the installed RNIC provider. NVIDIA allocations currently
use synchronized host staging for network transfer; direct GPU RDMA remains
under development. The [capability matrix](capabilities.md) separates these paths.

## Installed quickstart

After maintainer publication, use the README's [Quick Start](https://github.com/OrbitInfra/OrbiTE#quick-start)
commands on Linux. No GPU or CUDA/RDMA SDK is required to run the wheel or the
Rust example with only `io-uring`. The Rust installer needs Rust 1.85+, a C
linker and standard build tools; Python needs CPython 3.10–3.14 on x86_64 or
aarch64. Use a Python virtual environment. A kernel/security policy that blocks
io_uring is an error, not a skipped file demonstration.

The exact programs are [Rust example](https://github.com/OrbitInfra/OrbiTE/blob/main/examples/orbite-quickstart.rs) and
[Python example](https://github.com/OrbitInfra/OrbiTE/blob/main/python/orbite/quickstart.py). They start a child source process, exchange
its authoritative endpoint and epoch through a pipe, READ then WRITE a guarded
DRAM range over TCP, and check both payload and guards. A temporary local file
then receives a WRITE and a READ through io_uring, also with guards. They close
their owners and remove the file. This demonstrates API use, not durable storage
or discovery. Production consumers supply their own endpoint/epoch authority.

Successful output from either program:

```text
TCP: two-process READ/WRITE and guards verified
io_uring: DRAM/file READ/WRITE and guards verified
```

Release CI installs the Rust example from the `.crate` archive and runs Python
with `-I -m orbite.quickstart` from installed wheels and the rebuilt sdist.
It never substitutes a source-tree import. Before publication, download the
review artifacts using the [release procedure](releases.md#maintainer-publication-procedure).
