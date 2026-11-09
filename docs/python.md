# Python

The `orbite` package is a thin PEP 8 binding to OrbiTE's Rust owners. It is
independent of Mooncake's Python API; there is no `tent` or `mooncake.engine`
alias. Rust remains the primary interface.

## Install and configure

Wheels target CPython 3.10–3.14 on Linux x86_64 and aarch64. The maintainer
publishes releases; CI builds installable review artifacts. Python beta.1 is
`0.1.0b1`.

```python
import orbite

config = orbite.EngineConfig.from_json('{"bind_addr":"127.0.0.1:0","tcp":true}')
engine = orbite.TransferEngine(config)
```

`EngineConfig(**settings)` and `from_json(text)` use the
[OrbiTE JSON format](rust-api.md#configuration-loading). `from_file(path)`
reads a bounded JSON file; `from_env()` reads only `ORBITE_CONF`, containing
inline JSON or a file path. `TransferEngine()` captures that environment
configuration once. Passing a config bypasses the environment. Unknown fields,
wrong types and metadata-server options are errors. The snapshot is immutable.

## Registered buffers and remote segments

Consumers publish `endpoint()` and `source_epoch()` in their own registry.
Use that authoritative epoch when opening a peer; an endpoint alone does not
identify a source incarnation. No discovery or external metadata service is
part of OrbiTE.

```python
with orbite.TransferEngine(config) as source, orbite.TransferEngine(config) as client:
    src, dst = bytearray(b"hello"), bytearray(5)
    with source.register_buffer(src) as remote, client.register_buffer(dst) as local:
        with client.open_segment(source.endpoint(), source.source_epoch()) as peer:
            with client.allocate_batch(1) as batch:
                batch.submit(orbite.TransferOp.READ, peer, [
                    orbite.TransferRequest(local.address, remote.address, 5,
                                           transport=orbite.Transport.TCP)
                ])
                task, = batch.wait(5.0)
                assert task.state == orbite.TaskState.COMPLETED
                assert task.locally_drained and dst == src
```

`register_buffer(owner)` acquires and retains a writable contiguous buffer
export. `register_memory(address, length, owner=owner)` registers a subrange
of a retained exporter; a bare foreign integer is rejected. Engine-allocated
`Memory` can also be registered by address. `register_memory_ranges(ranges,
owners=...)` validates the whole bounded list before registration. Close
related aliases together with `unregister_memory_ranges(registrations)`.

CUDA exporters use `__cuda_array_interface__` v2/v3 or
`__dlpack__` plus `__dlpack_device__`. The driver validates the whole exported
extent, pointer and device; Python claims alone are insufficient. Read-only,
noncontiguous, out-of-range and wrong-device exports fail. CAI v3's stream and
DLPack's producer handoff establish readiness. The Python owner and consumed
capsule remain retained through drain, aliases and quarantine. A CPU wheel
reports `UnavailableError` before requesting a CUDA capsule.

```python
registration = engine.register_memory(tensor.data_ptr(), tensor.nbytes, owner=tensor)
```

## Batches, notifications and files

`Batch.submit(operation, target, requests, notification=None)` enqueues through
the Rust admission, selector and physical-owner paths. Requests carry a local
address, remote address (file offset for a file target), length, transport,
priority, optional policy name, absolute monotonic `deadline_ns` and intent.
`tasks()` observes; `progress()` advances a bounded step; `wait(timeout)`
waits with the timeout in seconds. `cancel(task_index)` cancels queued work;
running work still drains. A terminal result and physical quiescence are
separate: inspect `locally_drained` and `failure.remote_effect`.

`send_notification(peer, Notification(name, message))` and
`receive_notifications()` use bounded native queues. A `None` peer delivers
locally. File operations use `open_file_segment(path, permission)` and the same
batch methods with `Transport.IO_URING` or `Transport.GDS`. Providers must be
built and enabled. `FileSegment.info()` reports the extent; close drains file
I/O. `stats()` returns a read-only snapshot of engine counters and charges.

## Surface and defaults

| Surface | Public names |
| --- | --- |
| Construction | `EngineConfig`, `TransferEngine` |
| Retained owners / context managers | `Memory`, `RegisteredMemory`, `RemoteSegment`, `FileSegment`, `Batch` |
| Requests and results | `TransferRequest`, `TaskStatus`, `TransferFailure`, `Notification`, `MemoryOptions` |
| Enums | `TransferOp`, `Transport`, `Permission`, `Priority`, `IntentType`, `TaskState` |
| Exceptions | `Error` and one `<RustVariant>Error` subclass per native error variant |

Defaults come from Rust's typed configuration. Requests default to `UNSPEC`,
high priority, unspecified intent and no deadline or named policy.
`MemoryOptions` defaults to any local location, read/write permission and
automatic transport selection. Only implemented OrbiTE transport identities
are exported; a compiled-out provider raises a typed unavailable error.

All blocking native calls detach the GIL behind a panic barrier. Exceptions
derive from `orbite.Error`, including `ConfigError`, `PermissionDeniedError`,
`EpochMismatchError`, `ResourceExhaustedError`, `TimeoutError` and
`UnquiescedError`. Python argument/type errors retain normal Python exceptions.
Task failures retain their independent byte prefixes and remote-effect status.

Context exit invokes explicit close and surfaces errors. Dropping an unfinished
batch defers retirement; dropping an allocation cannot free memory used by a
registration or operation. A failed native cleanup retains owners and charges
and is never retried as a destroy. Timeout, cancellation and dropped Python
objects are not release receipts. Explicit unregister can report
`ResourceExhaustedError` while a native owner still holds the range.
`RemoteSegment.close()` releases that handle, not the peer identity; Rust's
per-peer retirement API remains available to Rust consumers.

`Transport` contains `UNSPEC`, `TCP`, `HP_TCP`, `SHM`, `RDMA`, `NVLINK`,
`MNNVL`, `IO_URING` and `GDS`. The removed `FABRIC` value 5 is unknown;
remaining numeric values are unchanged. A known transport that is not built
still reports `UnavailableError` when requested.

## Packaging and evidence

The Python-only crate uses PyO3 and maturin. A CPython-specific wheel rather
than abi3 preserves buffer protocol and CUDA exporter support across the tested
interpreters. Its exact core dependency pin keeps hidden owner helpers within
one release; other shared dependencies follow the repository's semver policy.
There is no independent coordinator in the binding.

The installed-wheel controls check real two-process TCP bytes and guards,
file segments, policy/intent/priority/deadline routing, exceptions, notifications,
GIL release, queued cancel and exporter retention under timeout and early drop.
The combined wheel runs CUDA exporter controls through a CPU driver shim.
The sdist is rebuilt, installed and tested separately. See
[qualification](qualification.md#python-wheels) for exact CI evidence.
Python GPU remains **Implemented; installed profile unqualified**; its
[installed gate](qualification.md#verification-queue) is written but unrun.
