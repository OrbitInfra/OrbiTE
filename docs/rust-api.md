# Rust API Guide

The package is `orbite` and the crate is `orbite`. Generate the full type
documentation with:

```sh
cargo doc --locked --no-deps --open
```

Add `--features rdma,cuda` in an environment with the
[build prerequisites](build.md).

## Beta.1 freeze candidate

This is the public surface and stability boundary for `0.1.0-beta.1`.
Hardware qualification is separate. Rust is the primary API.
The following names and their public methods/fields are the candidate; private
transport owners, native function tables and test shims are not public APIs.
Full signatures come from rustdoc for the chosen feature set, not from the
the independent OrbiTE API; TENT compatibility surfaces are out of scope.

| Area | Public surface |
| --- | --- |
| Construction | `TransferEngine`, `EngineConfig`, `RuntimeQueueConfig`; typed `new`, explicit `from_env/json/file`, config and stats queries |
| Requests and outcomes | `TransferRequest`, `TransferOp`, `Transport`, `Priority`, `FileIoElement`, `BatchReport`, `TransferFailure`, `RemoteEffect`, `Error`, `Result` |
| Registered backing | `HostMemory`, `MemoryBacking`, `RegisteredMemory`, `Permission`, `MemoryType`, `MemoryLocation`, `RangeLocation`; owned/external and range-batch registration, address resolution, unregister, guarded reads/writes |
| Peer and file authority | `SourceEpoch`, `RemoteSegment`, `RemoteRegion`, `TransferTarget`, `FileSegment`, `FileSegmentInfo`; endpoint/epoch discovery, open/refresh/close, file offset resolution and info |
| Async work | `Batch`, `BatchId`, `TaskState`, `TaskStatus`, `BatchState`, `BatchStatus`, `SubmissionStatus`; allocate, append/groups, seal, observe/progress, cancel, wait, drain and free; `next_poll_delay`, `wait_before_next_poll` |
| Notifications | `Notification`, `NotificationEffect`, `NotificationFailure`, `NotificationStatus`; attached, local and remote send, receive and delivery status |
| Selection | `IntentType`, `PolicyName`, `SegmentType`, `SelectionPolicy`, `TransportSelectorConfig`, `QpPool`; compiled policies and named-policy resolution |
| QoS | `QosConfig`, `QosPolicyFields`, `QosDegradedActions`, `QosTenantContract`, `QosRequestContext`, `QosUsage`, `EffectiveQosPolicy`; resolve and sample contracts |
| Credits and accounting | `CreditResource`, `CreditBalance`, `CreditAmount`, `CreditUpdateDisposition`, `ReceiverCreditUpdateV1`, `ReceiverCreditConfig`, `ReceiverCreditStats`, `ReceiverCreditIssuerStats`, `EngineStats`, `RuntimeQueueStats`; manual and automatic credit lifecycle |
| CPU providers | `HpTcpConfig`, `ShmConfig`, `MnnvlConfig`, `MnnvlHandleType`; SHM allocation/free (MNNVL settings exist without an active CUDA provider) |
| Linux topology | `Topology`, `NicEntry`, `NicType`, `MemEntry`, `TopologyMemoryKind`, `DEVICE_PRIORITY_RANKS`; local/discovered topology snapshots |
| `rdma` | `transport::rdma::{RdmaConfig, RdmaHostMemory, RdmaCudaMemory, RdmaProgressMode, SchedulingParams, NicLoadStats, QpReadDepths, RdmaStats}`; NIC load queries |
| Unstable `rdma-adaptive` (excluded from freeze) | `transport::rdma::adaptive_congestion_control`: `Config`, `Mode`, `PathState`, `Snapshot`, `Signals`, `DomainState`, `AdmissionDomain`, `Decision`, `Reason`, `Rejection`, `PathHandle`, `Permit`, `Classification`, `FailureScope`, `Outcome`, `classify_completion`, `classify_async_event`; hidden callable core, not a live worker policy |
| `cuda` | `cuda::{CudaMemory, RetainedResources, allocate_pinned_host, retained_resources}`, root `StagingLocation`, `StagingStats`, `NvlinkStats`; device/fabric/EGM allocation, external ownership, producer readiness, staging trim and IPC cache flush |
| `cuda::local_copy` | `PinnedHostRegistration`, `CopyDirection`, `LocalCopyBackend`, `LocalCopyConfig`, `LocalCopyDesc`, `ProducerEvent`, `LocalCopyEngine`, `LocalCopyStats`, `LocalCopyCompletion`; caller stream, descriptors, submit, event progress/wait and close |
| File and network features | `io-uring`: `IoUringConfig`, `IoUringStats`; `gds`: `GdsMode`, `GdsStats`; `fabric`: `FabricConfig`, `FabricProvider`, `FabricStats` |
| `metrics` | `metrics::{MetricsConfig, Metrics}`; engine metrics/status, enable toggle, JSON/Prometheus snapshots and writers |
| Offline diagnostics | `diagnostics::{BUILD_VERSION, BUILD_COMMIT, SCHEMA_VERSION, Outcome, Check, BuildInfo, Snapshot, version_snapshot}`; text/JSON rendering and exit code; `orbite diagnostics version` CLI |

### Freeze decisions

All public enums and every public struct with public fields are
`#[non_exhaustive]`. `TransferOp` is the sole exhaustive enum: its binary
READ/WRITE contract is intentional. CI enforces Clippy's `exhaustive_structs`
and `exhaustive_enums` lints with that single exception. Opaque structs with private fields already
require their constructors. Match enums with a wildcard and destructure field
records with `..`; construct configuration through defaults and field assignment:

```rust
use orbite::{EngineConfig, RuntimeQueueConfig};

let mut queue = RuntimeQueueConfig::default();
queue.max_dispatch_owners = 8;
let mut config = EngineConfig::default();
config.runtime_queue = Some(queue);
```

`Notification::new`, `NicEntry::new`, `MemEntry::new`, `CreditAmount::new` and
`ReceiverCreditUpdateV1::new` construct input records without struct literals.
Their public fields remain mutable. QP pools use `QpPool::default()` followed by
a unique name and any count/link overrides.

| Boundary | Freeze decision |
| --- | --- |
| Primary Rust helpers | `transport_installed`, `register_memory_ranges`, `Batch::observe_tasks`, `resolve_policy_name` and `discover_peer_epoch` are documented API. Installation is not availability on a peer; observational status is not a drain receipt; discovery never replaces consumer epoch authority. |
| Binding internals | Backing `release_owned`, registration `exclusively_held` and `shm_backing_name` are `#[doc(hidden)]`, outside the stability promise. The Python packaging crate pins the core to the exact same release because it uses these helpers. |
| Names | `NvlinkStats` matches `Transport::Nvlink`; `MemoryBacking` is the actual retained owner enum. `TopologyMemoryKind` describes topology entries and is distinct from registration `MemoryType`. Public TENT settings and transport-ID conversions are removed. `ReceiverCreditUpdateV1` names OrbiTE's credit-update schema. |
| Adaptive core | The callable `rdma-adaptive` module is `#[doc(hidden)]`, unstable and outside this freeze. It has no engine integration or consumer. Crate-private visibility creates dead-code/unused-import warnings; hiding the reference core preserves its tests without suppressing those lints. |
| Documentation | Missing docs are not a beta freeze blocker: adding documentation is non-breaking. The release-readiness audit found 668 all-feature Linux diagnostics. Before `0.1.0`, close the remaining findings and promote the CI audit to `-D missing-docs`, as required by the [release ladder](completion-plan.md#release-ladder). Ownership, receipts, epochs and units are documented at their affected entry points. |
| Native binding types | The deliberate cudarc stream coupling and raw owner-backed alternative below remain. GPU signatures are frozen candidates even though installed profiles remain unqualified. |

## Transfer requests

`TransferRequest::new(transport, local_address, remote_address, length)` starts
with `Priority::High`, the engine's default QoS contract and remote staging off.
Override them with `.with_qos(policy)`, `.with_priority(priority)` and
`.with_remote_staging(true)`; an explicit QoS policy supplies its own priority.
Use `Transport::Unspec` for [transport selection](transport-selection.md).
Public enums other than `TransferOp` are non-exhaustive, so matches outside
the crate need a wildcard arm.

Both ranges must lie inside one applicable registration, and `TransferOp`
selects READ or WRITE. Submission resolves and retains every backing allocation
before any transfer effect. Priority orders RDMA payloads on each worker; TCP and
NVLink accept every priority as advisory, and requests within a batch keep their
order.

## Engine and registration

`TransferEngine::new(EngineConfig)` starts a source control service (loopback by
default) and owns requester and source admission. Configuration is immutable per
engine. `engine.config()` borrows its effective settings.

### Configuration loading

```rust
use orbite::{EngineConfig, TransferEngine};

fn open_engine() -> orbite::Result<TransferEngine> {
    TransferEngine::new(EngineConfig::from_env()?)
}
```

`EngineConfig::from_json(text)` and `from_file(path)` take explicit input and
ignore the environment. `from_env()` reads only `ORBITE_CONF`, containing either
inline JSON or a regular UTF-8 file path; absent means typed defaults. Engine
construction never rereads it. Reuse a snapshot after changing or deleting its
input file. No loader mutates the process environment.

The root uses the typed `EngineConfig` field names. Transport fields may instead
be grouped under `transports`; a field cannot appear in both places. Provider
records use their typed field names and defaults. An optional provider object
opts in; `null` disables it. `hp_tcp.enable` and `shm.enable` remain explicit.
Duration fields have an `_us` suffix and take integer microseconds; fields already
naming a unit, such as `connect_timeout_ms`, retain that unit. Other sizes are
bytes except explicitly named KiB settings. MNNVL `fabric_domain` uses the
32-hex-digit `SourceEpoch` representation or `null`. Selector rules and QoS contracts
live under `selector` and `qos`; receiver budgets under `receiver_credit`.

```json
{
  "bind_addr": "127.0.0.1:0",
  "handler_timeout_us": 5000000,
  "transports": {"tcp": true, "shm": {"enable": false}},
  "selector": {"policy": [{"name": "host", "segment_type": "memory", "transports": ["tcp"]}]}
}
```

Unknown keys, duplicate keys, wrong types, malformed roots and out-of-range
values fail before an engine starts. Documents are bounded to 1 MiB and files
must be regular files. Removed metadata keys and legacy aliases are unknown
fields. Legacy environment variables have no effect; provider-runtime variables
such as `FI_PROVIDER` remain the provider's responsibility. Native fields require
their build feature. Typed Rust construction stays the primary interface.

| API | Contract |
| --- | --- |
| `register_memory` | Move host bytes into a registered range |
| `register_host_memory` | Register a range of an existing `Arc<HostMemory>`, retaining its allocation owner |
| `register_host_memory_batch` | Preflight a set of host ranges, then publish them together |
| `register_cuda_memory_batch` | Register CUDA allocations (`cuda` feature) |
| `unregister_memory` | Drop a registration reference; admitted users keep their backing |
| `resolve_local_address` | Resolve a contained registered range without adopting a raw pointer |

`Permission` distinguishes local-only, remote READ, remote WRITE and remote
READ/WRITE; repeated registration of a range keeps its first permission. With
RDMA host Direct policy, registration also prepares MRs on every usable context
before the range becomes visible, so it can fail when native resources are
exhausted; see the
[publication contract](rdma-cuda.md#eager-publication-and-cross-rail-slice-completion).

## Admission and QoS configuration

| Setting | Purpose | Guide |
| --- | --- | --- |
| `EngineConfig::runtime_queue` | FIFO/EDF dispatch window for all data submissions | [Dispatch window](async-batches.md#runtime-admission-and-dispatch-window) |
| `EngineConfig::qos`, `QosConfig::from_json(_file)` | Global/tenant/intent contracts; `resolve_qos` returns a reusable `EffectiveQosPolicy` | [QoS](async-batches.md#qos-contracts), [actions](async-batches.md#qos-admission-actions) |
| `EngineConfig::max_receiver_credit_sessions` | Sender credit gating; `activate`/`apply`/`deactivate_receiver_credit` for manual grants | [Credits](async-batches.md#receiver-credit-gating) |
| `EngineConfig::receiver_credit` | Receiver-side issuer; `connect`/`disconnect_receiver_credit` for automatic windows | [Automatic windows](async-batches.md#automatic-receiver-windows) |
| `EngineConfig::selector`, `TransportSelectorConfig::from_json` | Ordered transport selection rules (`selector.policy`) | [Selection](transport-selection.md) |

`EffectiveQosPolicy::fields()`, `effective_priority()` and `usage()` explain a
resolved policy and sample its budget.

## Memory type and location

`RegisteredMemory::memory_type()` returns `MemoryType::Cpu` or `Cuda` from the
retained backing, without locks or driver calls. `RegisteredMemory::location()`
returns `RangeLocation { address, length, location }` clipped to that
registration, and `HostMemory::location(offset, length)` /
`CudaMemory::location(offset, length)` check the owner subrange first. These
queries neither wait for producer readiness nor grant payload access.

`MemoryLocation::Cuda { device, context }` displays as `cuda:N`; the context
number is diagnostic, not a usable handle. `MemoryLocation::Cpu(Some(N))`
displays `cpu:N` and `Cpu(None)` displays `*`. Ordinary, external and portable
pinned host memory have unknown NUMA placement, and pinned DRAM stays CPU memory.

External CUDA adoption makes one `cuPointerGetAttributes` call (type, managed
status, context, device), resolves the device with `cuDeviceGet` and checks the
whole range with `cuMemGetAddressRange` in that context. Unknown attributes,
managed or host pointers and native errors fail adoption and never select CPU
access. This covers the owner-backed part of TENT's `Platform` `getMemoryType`,
`getPointerDeviceId` and `getLocation`; there is no general pointer classifier,
CPU page-residency scan or device-plugin ABI. See
[qualification](qualification.md#cuda-pointer-and-location).

## RDMA bandwidth and load

With `rdma`, `RdmaConfig::scheduling` configures `SchedulingParams` for
bandwidth-aware selection (on by default; `smart_selection = false` uses
round-robin in the first tier); see
[selection](rdma-cuda.md#bandwidth-aware-rdma-selection).
`TransferEngine::nic_load_stats()` returns initialized NICs without discovery:
rate estimates in **bytes per second**, `link_speed_gbps` in Gbit/s,
`inflight_bytes` (grant admission), `posted_bytes` (native payload debt) and
cumulative completed bytes, busy time and sample counts. Priority settings
(`deadline_bw_arbitration`, `priority_promotion_timeout`,
`priority_promotion_per_entry`, `shared_quota_shm_path`,
`slot_rotation_interval`) are described in
[request priority](rdma-cuda.md#request-priority-and-bandwidth-arbitration).

## Peer authority and file segments

```rust
let peer = requester.open_segment(
    source.endpoint(),
    source.source_epoch(),
    Duration::from_secs(1),
)?;
```

The expected `SourceEpoch` must come from the authoritative source owner;
descriptors identify regions but never replace that authority, so a restarted
process at the same endpoint cannot be reached through an old handle.

### Discovery boundary

OrbiTE transfers bytes between known peers. The consumer owns discovery,
membership, KV indexes and external metadata stores. Publish the source's
`endpoint()` and `source_epoch()` in your own registry, then pass that
authoritative epoch to `open_segment(endpoint, expected_epoch, timeout)`.
For example, OrbitKV maintains its own etcd membership and passes
`transfer_endpoint` and `native_source_epoch` over its own gRPC. OrbiTE's
P2P handshake supplies descriptors, capabilities and registration keys for
that identity; it does not discover cluster members.

There are no metadata-server configuration keys. Unknown keys fail with
`Error::Config`; no metastore URI or credential is resolved. The same immutable
loader serves Rust and Python.

When membership says a peer left, call
`engine.close_segment(endpoint, expected_epoch, timeout)`. It immediately fences
new requester admissions to that identity, drains already admitted transfers
and notifications, then retires its transport cache entries. Existing
`RemoteSegment` snapshots return `Error::PeerClosed` on submit or refresh.
Other peers remain usable. Obtain a replacement epoch from its authoritative
owner and call `open_segment` again; never edit an old descriptor's epoch.
This is outgoing retirement, not an authenticated ban on inbound clients.

A drain timeout returns `Unquiesced` and retains backing and budget charges.
You may wait again while admitted work drains. Once native cleanup starts,
its result is sticky: an uncertain destroy is never repeated. The timeout
bounds waits, not a native call that cannot be preempted. Automatic receiver
windows close with a receiver receipt; unresolved credit charges remain held.
`EngineConfig::max_peer_segments` bounds simultaneously retained endpoint/epoch
identities (default 1024, maximum 4096), not cumulative peer incarnations.
Every `RemoteSegment` clone, admitted operation, alias, retry, notification and
cleanup keeps its identity charged. Once the last reference is gone, the next
table lookup or stats snapshot reclaims an open entry, or a retired entry whose
cleanup succeeded. A timed-out drain still owes cleanup; failed native cleanup
never becomes reclaimable. Capacity pressure returns `ResourceExhausted`.

After a retired identity is reclaimed, the consumer may explicitly call
`open_segment` with the same epoch from its authoritative owner. Describe still
rejects a wrong epoch. A descriptor belongs to the requester that opened it;
using it with another engine returns `InvalidConfig`. Open it on that engine
instead. This prevents a borrowed snapshot from recreating admission implicitly.

`EngineStats::peer_segments` counts all charged identities; `live_peer_segments`
and `retired_peer_segments` partition that count by admission state.
`unquiesced_peer_segments` counts the retired subset with a timed-out drain or
failed cleanup. `reclaimed_peer_segments` is the cumulative reclaimed-entry
count, and `active_peer_operations` counts outstanding admission permits. These
counters are observations, not release receipts. Dropping a descriptor or calling the
Python `RemoteSegment.close()` only closes that handle; reopening
the same live source. The stronger Rust operation deliberately remains separate.

For a local file, `open_file_segment` takes a `file://` name and a permission;
`file_segment_info` returns the cached descriptor, `refresh_file_segment` re-reads
the size from the same fd, `resolve_file_offset` checks a range, and
`close_file_segment` closes every alias and waits for drain. See
[file segments](storage.md).

## Reports

`submit_and_wait` returns a `BatchReport` with completed request and byte counts,
failure details, notification status and local drain status. The completed prefix
is not a transaction. `RemoteEffect::Unknown` means an attempted WRITE may have
changed peer bytes, and never authorizes replay. `TransferFailure.affected_requests`
counts the consecutive tasks that share a failure from `request_index`; only later
requests were never attempted. A multirail or remote-staged request counts only
after all its slices or chunks complete, but earlier slices may already have
changed bytes. For asynchronous ownership see [async batches](async-batches.md).

For explicit progress, call `engine.progress_batch(batch.id()?)?`. A status
query observes the published result. `EngineConfig::enable_progress_worker`
enables event-driven turns and deferred free. Use
`Batch::submit_with_notification_targets` to track deliveries independently
of the payload result. See [explicit progress](async-batches.md#explicit-progress)
for backoff, queue wakes, bounded retries and owner retirement.

## Native providers

The CUDA feature accepts compatible cudarc 0.19 releases. Rust 1.85 builds
use 0.19.0 in the checked-in lockfile. Consumers resolving a fresh lockfile
on 1.85 can select it with `cargo update -p cudarc --precise 0.19.0`.
Consumers using cudarc 0.19.8 share one registry package with OrbiTE on
Rust 1.88 or newer, as required by that release's libloading dependency.

OrbitKV deliberately passes its `Arc<cudarc::driver::CudaStream>` as the
lifetime owner for local copies. Code that names that type follows cudarc's
semver. The underlying `unsafe LocalCopyEngine::bind` accepts a
`*mut std::ffi::c_void` and an `Arc<dyn Send + Sync>`. A caller with its own
CUDA binding can supply its raw stream and owner directly. The same contract
applies to `ProducerEvent::from_raw`, so a future C adapter can retain its
own handles. [Local copy ownership](local-copy.md)

`transport::rdma::RdmaConfig` selects discovery and pins, host Direct or staged
memory, progress mode (`Polling` by default, or `EventDriven`) and native budgets.
The `cuda` module owns device and pinned-host allocations and synchronized copies;
device transfers use staging or explicit GPUDirect RDMA. Unsafe
`CudaMemory::record_producer_stream` and `HostMemory::record_producer_stream`
hand already-queued producer work to transfers; read the
[producer contract](rdma-cuda.md#producer-stream-readiness) before using them.

## Close and drain

`close_source` and `close_requester` fence one direction; whole-engine `close`
fences both, drains accepted work and closes shared native resources. A timeout
never completes pending native work, and `Error::Unquiesced` keeps the real
resources and charges when cleanup could not be proved. With `cuda`,
`flush_nvlink_cache(timeout)` closes the cached imports present when it starts
while admission stays open; see
[the cache contract](cuda-ipc.md#cross-submission-relocation-cache).

Runnable examples: [TCP](../examples/tcp_roundtrip.rs),
[external allocation](../examples/external_host.rs) and
[async batches](../examples/async_batch.rs).

The [Python API](python.md) uses the same Rust owners through the optional wheel
boundary; Rust consumers do not compile or load Python.
