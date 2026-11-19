# Frozen CPU comparison

P0.1 supplies one executable behavior trace for the CPU/TCP profile. The public
OrbiTE Rust API, the frozen native TENT C ABI and the installed OrbiTE Python API
run in separate processes. Each runner uses real host backing and loopback TCP
between its own source and requester engines. The comparison checks correctness
within each runtime.

## Reference and build boundary

The machine-readable [contract](../tests/reference/contract.json) pins Mooncake v0.3.14 at
`e1ea5d31bb5e46ff2df9f90e7c2fbaf52fcdee8c`, its pybind11 gitlink and YLT archive
revision/SHA-256. The runner checks clean reference source, both Git revisions
and the archive hash before building. Compiler commands, effective CMake cache,
binary/library hashes and loaded dependencies remain in external artifacts.
Native code is compiled from the unmodified reference; no C++ implementation
is copied into OrbiTE.

The runner configures Mooncake's root CMake project, which defines the CLI11
header target needed by TENT, and builds `tent_shared`, the frozen `tebench`
executable and their dependencies. Upstream links both classic and TENT code
into that executable. The byte gate selects the TENT backend explicitly.
The reference build disables Mooncake Store and its Rust bindings, CUDA/HIP,
runtime metrics and YLT IBV. Upstream
CMake can still compile/link discovered RDMA or HTTP support. The native driver
explicitly enables TCP and disables every other transport, selects P2P metadata,
uses four RPC threads, and disables merging, the runtime queue and background
progress. Compiled provider availability does not qualify those paths. No
external metadata service or accelerator is used by this trace.

Reference sources, libraries and build output stay outside the OrbiTE checkout
and OrbitKV's staged runtime. The runner removes inherited Mooncake configuration
and native loader overrides. It verifies that native Mooncake/ASIO dependencies
resolve inside its private build and that the Rust executable has none of those
dependencies. Normal Cargo builds do not build or load TENT.

## Trace and observations

[`cpu-v1.trace`](../tests/fixtures/cpu-v1.trace) is a small deterministic source
fixture. [`cpu-v1.expected`](../tests/fixtures/cpu-v1.expected) defines exact byte,
descriptor, completion and notification observations from its specified inputs.
Every driver compares against that expectation. The native comparison also
checks the actual Rust and TENT outputs against each other.

| Contract | Trace coverage | Remaining boundary |
| --- | --- | --- |
| F04 host registration | Existing backing, single and batch duplicate registration, first permission retained during subsequent WRITE, partial/final unregister and descriptor refresh | Failed provider free, overlapping native registrations and retained DMA |
| F05/F07/F08 CPU movement | Noncontiguous batched READ, offset WRITE, exact bytes/sentinels and completed byte counts | Native error mapping, partial posts, cancellation and failover |
| F08 batch lifetime | Drivers allocate/submit/poll/free; the first READ appends two groups to one capacity reservation; two independent READ handles retire in reverse order across a non-final source unregister; terminal polling repeats without redelivery | Asynchronous multi-target hooks and merged task aliases |
| F11 notifications | Explicit local/remote, attached READ/WRITE, empty submission, deliberate repeats, empty names/messages and empty polls | Ambiguous native receipts, provider fallback and durable/deduplicated delivery |
| Selected F09 C output | Native notification result reuse, reset on free and repeated free | Frozen TENT output contract only; OrbiTE C ABI is out of scope |

The text format starts with `ORBITE_CPU_TRACE 1`. `BUFFER` selects a named
source/local backing with length and `pattern` (index modulo 251) or one fill
byte. `REGISTER`, `UNREGISTER` and `DESCRIBE` change or inspect registration.
`REGISTER_BATCH name r count` adds `count` references to an already registered
range via one real batch call; later single unregister calls must release every
reference. New duplicate descriptors in one native batch are outside this trace.
`FILL` and `BYTES` specify offsets/lengths. `READ`/`WRITE` take a label,
comma-separated `local_offset:remote_offset:length` slices, notification name
and message. `READ_SPLIT` submits each slice separately into one allocated
batch, without notification. The Rust driver seals after append.
`READ_PAIR_UNREGISTER` submits two single-slice READs into disjoint local
ranges using separate batch handles, then drops one duplicate source
registration. The drivers wait for and free the second handle before the first.
Different slice lengths expose receipt swaps; `BYTES` checks both destinations
and their untouched guards. Both handles must coexist, but either transfer may
already have finished at unregister. The Python driver waits for physically drained task receipts via `Batch.wait` and checks
counts and byte totals from the returned tasks. All drivers observe terminal
status repeatedly and free each batch before cleanup. If the native replay fails before drain,
the isolated process exits without destroying a potentially active TENT engine.
`~` disables the attached notification; `-` means empty strings or an empty slice list. `NOTIFY` selects local/remote delivery; `TAKE` drains
the source inbox. Fixtures use bounded, valid ranges and C-string-compatible
messages. Random addresses, IDs and timing are excluded from observations;
each driver checks that descriptors identify its actual backing.

Lines beginning `OBS` are compared exactly. Native errors, failed processes,
timeouts, missing observations, extra messages and missing cleanup fail the run.
Seven comparator controls deliberately corrupt bytes/counts/status, swap the
two batches' byte counts, or remove/add observations and require rejection. These are comparator checks,
not network, epoch or lifetime fault injection. The existing Rust tests retain their real
restart, partial-body, close/drain and lost-receipt controls; running this shared
trace does not qualify corresponding behavior in unmodified native TENT.

## Run the gates

Use Linux, Rust 1.85+, Python 3.11.8+, Git, CMake, a C++20 compiler and permission
to bind loopback sockets. Select a **new external directory for each run**;
the runner refuses to overwrite evidence or put it inside a source checkout.

The fast source gate needs no native libraries or downloads:

```sh
python3 tests/reference/run.py --rust-only --output /var/tmp/orbite-fixture-001
```

It explicitly reports `passed-rust-fixture-only`; it cannot pass the native
comparison. Stable/MSRV CI runs this gate alongside ordinary Cargo checks.

For the real native comparison on Debian/Ubuntu, install the build dependencies
listed in [the native CI workflow](../.github/workflows/native-cpu.yml), then:

```sh
python3 tests/reference/run.py --fetch-reference --output /var/tmp/orbite-native-001
```

This explicitly downloads the fixed public Git commit/submodule and archive.
For an already provisioned clean reference and verified archive:

```sh
python3 tests/reference/run.py \
  --tent-source /var/tmp/tent-reference \
  --ylt-archive /var/tmp/45c6463c0afbcc46262e373effa30635be3846a0.tar.gz \
  --output /var/tmp/orbite-native-002
```

The full run reports `passed-cpu-trace` after both trace runners exit
successfully, all 29 observations match and the tebench byte controls pass.
Its manifest preserves exact inputs,
commands, command outcomes, binary hashes, native dependency paths/hashes,
observations, comparator controls and provenance. Failures keep their logs and
mark the manifest failed. Timeout kills are failures, never drain receipts.
The extended, release and manually dispatched native CI runs upload this evidence,
including failed runs. Source commit provenance and a clean reference do not turn an untested
hardware or API surface into an accepted profile.

### Frozen tebench byte checks

The unmodified TENT `tebench` initiator writes two 4 KiB blocks to a guarded
DRAM range over loopback TCP with `write_seed`. A new `read_verify` process
checks all 8,192 bytes. Between initiators the target checks its payload and both
32-byte guards, then flips one bit in the final byte. The next verification must
abort with TENT's byte-mismatch diagnostic. The target restores the original
byte and a final verification must succeed before it unregisters the buffer.
Each successful initiator must report a completed workload row. The manifest
records the executable hash, resolved libraries, commands, exit statuses and
corruption offset. The workload checks native reference behavior and supplies
no performance measurement for OrbiTE.

### Installed Python wheel

Use a CPython interpreter matching the wheel's tag. The runner creates a fresh
virtualenv in the external output directory, installs the supplied wheel with
`pip --no-index --no-deps`, then runs the driver there in isolated Python mode.
The driver checks its import path inside that virtualenv. The manifest records
the wheel SHA-256, the interpreter version, commands, input hashes and the same
seven comparator controls. The wheel trace executes without building Rust or TENT.

```sh
python -I tests/reference/run.py --python-wheel "$WHEEL" \
  --output /var/tmp/orbite-wheel-trace-001
```

The result `passed-python-wheel-trace` requires all 29 observations, including
duplicate registration and permission retention, guard bytes, independent batch
receipts, empty batches, and notification order and multiplicity. Live aliases must keep the Python
exporter pinned, and final unregister must allow resizing. Before emitting
cleanup the driver closes every registration, batch and segment guard, checks
the memory and batch charges on both engines, and closes both engines.
The wheel matrix runs this
gate on CPU + io_uring and combined wheels and on the rebuilt sdist wheel.
See [wheel qualification](qualification.md#python-wheels) for its scope.

## Isolated RDMA geometry function

P4.12g adds a separate valid-input comparison for frozen TENT's
`RdmaSlicePlan` and `planRdmaSlices`. The probe reads the exact Git blob at the
contract's pinned revision and compiles the unchanged structure/function bodies
inside a small C++ wrapper. It compares sixteen Rust plans, including short-tail,
exact-quarter, MiB and capped-count cases. Inclusive-quarter and missing-fold
native mutations must disagree. Frozen header/function bytes, commands, compiler,
inputs, outputs and binary hashes stay in a new external evidence directory.

```sh
python3 tests/reference/rdma_geometry.py \
  --tent-source /var/tmp/tent-reference \
  --output /var/tmp/orbite-rdma-geometry-001
```

An optional `--binary /path/to/frozen-libtest` executes the same exact unit test
from a recorded release binary. Otherwise the probe uses Cargo with the RDMA
feature and requires the CI binding dependencies. Native CI runs this after the
full CPU transport trace and uploads a separate geometry artifact.

This probe executes only the isolated native function. It does not execute
native TENT RDMA transport, establish invalid-input parity, qualify hardware or
measure throughput. The separate two-RNIC libtest profile and its ownership
controls are recorded in the
[frozen geometry evidence](https://github.com/OrbitInfra/OrbiTE/blob/9e1fdeaa9f7cd6c03e30aab2dff19a00f0a44578/docs/rdma-cuda.md#larger-requests-folded-tails-and-eight-parent-admission).

## Qualification limits

This comparison covers valid-input CPU/TCP behavior through the three drivers.
Full P0 coverage, native errors and configuration, physical two-host tests,
RNIC/GPU/storage and installed consumer qualification remain open.
OrbitKV-specific source epochs and source-close
extensions need their own audited adapter controls. Use the
[completion plan](completion-plan.md) for subsequent delivery and independent
acceptance; there is no second execution queue here.
