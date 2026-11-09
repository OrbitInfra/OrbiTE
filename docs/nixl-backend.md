# NIXL backend design

**Proposal only.** No plugin, C ABI or NIXL qualification exists yet. The target
is an in-tree backend in `ai-dynamo/nixl`, under `src/plugins/orbite`, rather
than a fork or a connector maintained inside each inference engine. OrbitKV's
upstream targets are vLLM and SGLang; the NIXL path would let vLLM
`NixlConnector`, SGLang's NIXL backend, Dynamo and LMCache P2P select OrbiTE.
This is an integration goal, not a claim that those consumers have tested it.

This review pins [NIXL main at `540fae3a`][nixl] (2026-11-08, whose Meson
project version is `1.6.0`), following the maintainer's v1.5.2 survey. The
[South Bound guide][guide], [backend header][backend], [plugin entry points][plugin],
[Mooncake backend][mooncake] and [contribution rules][contributing] are the
contract reviewed here. Recheck that pin before implementation. OrbiTE's
design/oracle baseline remains Mooncake TENT v0.3.14; NIXL is neither a new
runtime dependency of the Rust crate nor a replacement for that baseline.

The [work order](completion-plan.md#current-focus) puts the measured direct-I/O
WRITE tail and small-WRITE handoff before implementation. The maintainer must
approve the upstream design issue first. Nothing in this document authorizes
posting it, publishing a package or opening an upstream PR.

## Owner and capability boundary

One NIXL backend instance owns one `TransferEngine`. The C++ adapter converts
STL descriptors and statuses; Rust's existing registrations, `RemoteSegment`,
`Batch`, admission, progress and transport owners do the work. There is no
plugin payload thread, forwarding queue, second scheduler or retry loop.
`enableProgTh` and `pthrDelay` map to the existing optional progress worker and
its validated fallback interval; they do not disable existing transport workers.
NIXL thread-safety modes do not relax Rust's locking or lifetime rules.
Take one explicit OrbiTE JSON configuration through backend creation parameters;
reject unknown plugin parameters. Do not reinterpret NIXL agent names as
addresses, read TENT environment aliases, or mutate the process environment.
NIXL supplies no standard per-request OrbiTE intent or policy field. Initially
use configured defaults and reject unrecognized nonempty `customParam` data;
do not silently claim inference-intent propagation. P/D arbitration needs an
agreed request mapping before that integration can be qualified.

Propose `ORBITE` for memory transfers: `supportsRemote=true`,
`supportsLocal=true` through a self connection, and `supportsNotif=true`.
Advertise DRAM always and VRAM only when CUDA support is built and available.
Capabilities are promises to validate requests, not a claim that every
advertised memory range has every transport. Compiled transport policies and
owner-derived capabilities still select the route; no adapter fallback is added.

| NIXL memory or pair | Existing OrbiTE path and restriction |
| --- | --- |
| `DRAM_SEG` | Retained `HostMemory` and registry ranges. TCP; opt-in SHM for actual shared mappings on the same boot identity; RDMA when installed. HP-TCP and fabric remain eligible only through their existing configured policies. |
| `VRAM_SEG` | Driver-validated `CudaMemory`, allocation/context lease and range loans. RDMA GPU Direct or staging; TCP through staging; NVLink for eligible same-host IPC allocations; MNNVL only for fabric-exportable backing and an explicit fabric capability. Neither IP addresses nor a device ordinal prove reachability. |
| DRAM ↔ VRAM | The existing staged TCP/RDMA or eligible CUDA/pinned-memory owners. Ordinary pageable DRAM is not silently relabeled pinned memory or CUDA IPC backing. |
| `FILE_SEG` ↔ DRAM/VRAM | Local `segment::file::Entry`, io_uring and CUDA staging, or eligible GDS. Native GDS alignment, strict explicit hints, completed-prefix results and fallback selection retain their current meaning. No remote file service. |
| `BLK_SEG`, `OBJ_SEG`, non-NVIDIA device memory | Not advertised; return `NIXL_ERR_NOT_SUPPORTED` before registration or posting. |

SHM cannot turn an arbitrary NIXL `malloc` allocation into shared memory. Use
TCP for that range. SHM tests and consumers need an actual OrbiTE shared
allocation, retained across registration, or a separately validated import of
the same mapping. Copying the allocation at registration would break subsequent
updates and is not an implementation option. Likewise an ordinary CUDA pointer
does not establish MNNVL exportability. NIXL callers must finish producing bytes
before posting; NIXL's descriptor has no CUDA producer event. Do not synchronize
an arbitrary caller stream implicitly. A later explicit event extension would
use `CallerSyncEvent`, not weaken this prerequisite.

### Local storage profile

NIXL's [file convention][files] fits OrbiTE's offset-addressed file segments,
but its capability flags are backend-wide. `getPublicData` is requested for
registrations of a remote-capable backend; advertising FILE there would imply
remote metadata that OrbiTE does not implement. Propose a local-only
`ORBITE_FILE` entry point in the same plugin directory and bridge, following
the POSIX/GDS storage model: `supportsLocal=true`, `supportsRemote=false`,
`supportsNotif=false`. Each instance still owns just one engine. Ask upstream
to agree to this split before adding the second entry point.

Exactly one side of a file transfer must be FILE. NIXL READ moves remote-list
bytes to the local list; WRITE moves the reverse way. Normalize these to memory
plus file offset once, reversing the OrbiTE operation when FILE is on the local
list. Test all four combinations. Descriptor pairs preserve order, lengths and
guards; only adjacent file extents may become a `file_vectored` operation.
Reject file↔file, attached notifications and requests to another agent.

Path registration accepts only modes whose semantics are implemented. NIXL
`ro`/`rw`, direct/sync flags and borrowed-fd mode cannot silently change access
or durability: unsupported flags return `NOT_SUPPORTED`. Borrowed fds require
a checked `dup` into the existing file Entry; re-resolving `/proc/self/fd` or a
path could open a different inode. This needs a small internal fd-adoption
entry point, not a storage executor. `queryMem` reports supported file facts
from that Entry and returns `NOT_SUPPORTED` for unsupported query fields.

## South Bound API correspondence

These are proposed mappings for every virtual method in the pinned header,
including the optional defaults. No new Rust public API is promised here.

| NIXL call | OrbiTE owner and adapter action |
| --- | --- |
| Constructor / destructor | Validate a captured OrbiTE configuration; create one engine and bounded handle storage. Close uses existing source/requester drain. The void destructor follows the retention rules below, never frees unresolved owners. |
| `supportsRemote`, `supportsLocal`, `supportsNotif`, `getSupportedMems` | Immutable memory/storage profile above, filtered by built capabilities. Unsupported combinations are rejected before payload admission. |
| `registerMem` | Checked address/length/type/permissions → retained backing and `register_memory_ranges`; CUDA uses native allocation/device verification. FILE → existing file Entry. Duplicate and overlapping registrations retain the registry's authority/alias rules. |
| `deregisterMem` | Fence the final registration's lookup, then establish its backing/native-resource release receipt. `unregister_memory` alone is not that receipt. Pending or uncertain retirement keeps the registration lease and charge. |
| `getConnInfo` | Bounded encoding of native `endpoint()` and `source_epoch()`, backend/profile identity and metadata format. No cluster discovery or external store. |
| `loadRemoteConnInfo` | Validate bounded input; bind the NIXL agent name to an endpoint plus owner-issued epoch. A changed identity fences the old one rather than retargeting prepared requests. |
| `connect` | `open_segment(endpoint, expected_epoch, timeout)` and existing native Describe. Keep its requester-owned identity token. Self connections use this engine's actual epoch. No TENT framing or autodetection. |
| `disconnect` | Fence with the existing `SegmentManager`/`close_segment` phases, then drain and retire only that identity's connections/imports. Repeated observations may wait for drain; a failed native destroy is never retried. |
| `getPublicData` | Serialize only the registered range's identity, bounds, memory kind and permissions plus the endpoint/epoch binding. Native Describe remains authoritative for registration generations, transport keys and capabilities. No FILE paths or fds are published remotely. |
| `loadRemoteMD` | Validate agent/epoch/registration identity and bounds against the retained native descriptor; create a lease, not another transport registration. A range or generation mismatch fails before admission. |
| `loadLocalMD` | Retain the local registration or file Entry for self/storage transfers. A distinct metadata wrapper references the same owner, so unloading it cannot deregister the source accidentally. |
| `unloadMD` | Fence this imported metadata lease against new prepares/posts and release its lookup reference. Existing admitted requests retain their peer/registration permits; peer invalidation additionally fences the whole epoch. Wrapper removal is not physical release. |
| `prepXfer` | Validate both lists, permissions, equal total lengths, all checked range arithmetic and descriptor/byte limits. Retain immutable descriptors, registrations and peer identity. No payload I/O. Policy compilation stays in engine construction. |
| `postXfer` | Admit a fresh `Batch` execution through the existing submit path, then seal. Preserve physical-owner merging, aliases, QoS, credits, policy and deadline. Copy bounded notification data before returning; never retain pointers to call-scoped optional arguments. |
| `checkXfer` | One bounded existing `progress_batch` step and observation of task/notification outcomes and physical drain. Use the status table below, never infer quiescence from a timeout or a task's logical state. |
| `releaseReqH` | Request queued cancellation, fence repost, and poll release nonblockingly. Pending release returns a **negative** status; `IN_PROG` is not safe here. Abandonment transfers ownership to the existing deferred batch retirement path. |
| Both `prepMemView` overloads, `releaseMemView` | Keep default `NOT_SUPPORTED` for prepared strided views initially; descriptor lists still work. The void release is a no-op for an absent view. Do not advertise an unimplemented optimization. |
| `genNotif` | Existing bounded notification send, endpoint/epoch permit and exactly-once logical delivery. Standalone notification failure is not permission to replay a transfer. |
| `getNotifs` | Drain existing notification queues into NIXL `(agent, bytes)` pairs. The envelope carries a validated sender identity and length, mapped to its current peer lease; reject oversized or stale envelopes. Preserve binary payloads. |
| `queryMem` | Local storage facts as above; `NOT_SUPPORTED` for memory queries without an existing owner-backed answer. Never invent capacity or topology. |
| `estimateXferCost` | `NIXL_ERR_NOT_SUPPORTED`; measurements do not justify a cost model yet. |
| `supportsTraceContext` | `false` initially. Do not retain `traceContext` or claim cross-peer tracing support. |

Inherited `getInitErr`, `getType`, `getCustomParams`, `getInitParam` and
`setInitParam` keep NIXL's base-class behavior. `getTelemetryEvents` drains
its bounded base queue; use `addTelemetryEvent` only for supported lifecycle
events derived from existing stats, with no payload addresses or invented
values. The plugin factory implements `create_engine`, `destroy_engine`,
`get_plugin_name`, `get_plugin_version`, `get_backend_options` and
`get_backend_mems`, exposed through `nixl_plugin_init`/`nixl_plugin_fini`.
Creation failure sets the expected init failure/null result, never an exception
crossing the plugin factory.

Preparation may do bounded control-plane work. `postXfer`, `checkXfer` and
request release must not wait for payload completion. Verify that withheld
CUDA readiness and CQEs cannot block those calls; any required asynchronous
continuation belongs in the existing batch worker. A successfully drained
execution may be reposted only after revalidating all retained identities and
resetting execution/notification state. Failed, canceled or uncertain executions
cannot be reposted, even if NIXL calls `postXfer` again. There is no plugin retry.

NIXL notifications are binary while OrbiTE's `Notification` fields are UTF-8.
Use a bounded versioned envelope with sender endpoint/epoch and base64 payload;
charge the expanded length before allocation and check it against notification
limits. Decode losslessly, including embedded NUL/non-UTF-8 bytes, and reject
malformed or stale envelopes. Matching the loaded peer identity is a generation
fence, not authentication; both deployments remain cooperative-peer systems.

## Completion, invalidation and allocation lifetime

The bridge retains two separate facts: logical outcome (including exact prefix
and `RemoteEffect`) and physical release receipt. Successful transfer status
requires every public alias to succeed, required notifications to finish, and
the execution's local loans to drain. `Batch::free()` can defer retirement;
its success alone is **not** a receipt. Registration release additionally checks
source-side borrowers, cached native resources and staging/context loans.

| OrbiTE outcome | Proposed NIXL status and retention |
| --- | --- |
| Prepared, not posted | `NIXL_ERR_NOT_POSTED`; no payload effect. |
| Admitted/queued/running, or outcome known but drain pending | `NIXL_IN_PROG` from `checkXfer`; retain all owners. Release returns a negative error until safe. |
| All tasks and required notification succeeded, locally drained | `NIXL_SUCCESS` from `checkXfer`. Request release succeeds only after its remaining resource-retirement checks. |
| Queued cancellation, proven drained | `NIXL_ERR_CANCELED`; all merged aliases share the owner's result. Running I/O still drains. |
| Invalid range/config/type | `NIXL_ERR_INVALID_PARAM`; unsupported capability → `NIXL_ERR_NOT_SUPPORTED`; permission failure → `NIXL_ERR_NOT_ALLOWED`. |
| Epoch/generation mismatch or removed metadata | `NIXL_ERR_MISMATCH` / `NIXL_ERR_NOT_FOUND`; never bind the old handle to a new peer. |
| Resource exhausted | `NIXL_ERR_BACKEND` plus the bridge's typed detail; no unbounded overflow allocation. NIXL has no matching resource-exhausted status. |
| Provably unposted disconnect | `NIXL_ERR_REMOTE_DISCONNECT`, retaining the unposted/effect-known detail. A status code by itself never authorizes replay. |
| Short/failed I/O, timeout after admission, unknown WRITE effect | Terminal failure, never `SUCCESS`; after ordinary drain return `NIXL_ERR_BACKEND` with the exact prefix/effect in the native result. No plugin retry or cross-backend replay. |
| `Unquiesced`, including failed native cleanup | Sticky `NIXL_ERR_BACKEND`; retain owners, charges and code/library lifetime. Neither `CANCELED` nor `NOT_POSTED` describes this case. |

NIXL cannot encode OrbiTE's full error or an independent drain flag in one
status. Keep those details in the bridge result, report bounded diagnostic
counters, and do not promote an error to success to make a handle disappear.
A negative terminal status is an outcome, not authorization to reuse memory.

### Integration gaps that need upstream agreement

The pinned core has three relevant behaviors, beyond the abstract guide:

1. [`releaseXferReq`][agent] treats a negative active-release result as
   `NIXL_ERR_REPOST_ACTIVE` and retains the handle, but treats any nonnegative
   result as released. Therefore `releaseReqH` must never return `IN_PROG`
   while owners remain. Once status is terminal, the
   [`nixlXferReqH` destructor][request] ignores the release result.
2. [`nixlLocalSection::remDescList`][memory-section] ignores `deregisterMem`'s
   result, removes metadata and returns success. Registration rollback and
   destructor paths also ignore cleanup results. OrbiTE cannot use that
   success as an allocation-release receipt.
3. Remote invalidation removes the remote section and calls `disconnect`,
   but erases its backend mapping even when disconnect fails. The plugin must
   fence and retain identities independently of those C++ containers. NIXL's
   expired remote-section check helps reject stale posts; it does not drain DMA.

**Prerequisite for implementation acceptance:** agree upstream on propagating
deregistration/release failures and retaining a pollable retirement token,
including partial registration rollback and agent destruction. A terminal-error
request must still have a path to observe retirement. No native failure may be
turned into successful deregistration by the core. This is a NIXL library
contract issue to discuss in the design issue, not a proposed vLLM/SGLang fork.

NIXL passes raw application pointers, not allocator owners. For borrowed
registration, the caller must keep the allocation and CUDA context alive until
successful deregistration after physical retirement. Retaining a C++ metadata
object or a Rust `Arc` containing its address does **not** retain that allocation.
Where an actual allocator lease is available, the bridge accepts retain/release
callbacks and releases it only on a receipt. For borrowed pointers, the NIXL
contract above must keep the caller's obligation observable. With uncertain
cleanup it lasts until process teardown; no plugin can prevent an application
from violating that obligation. Do not claim support for freeing such memory
after a failed release or agent destruction.

Remote unload/invalidate fences admission first. Accepted transfers keep their
`RemoteSegment`, registration/range loans, staging and charges until the backend
proves release. `close_segment` owns peer cleanup and its sticky failure state;
the adapter does not retry destroys or maintain another connection cache.
An explicit new epoch opens a different identity; stale prepared requests fail.

The void engine/plugin destructor cannot return `Unquiesced`. After a bounded
close attempt it retains unresolved native owners and pins every library
containing their destructors or callbacks. The plugin DSO must not be unloaded
while those callbacks can execute. Pinning is charged per retained engine,
not per retry; failed destroy stays one-shot. This does not manufacture an
allocation lease for borrowed memory. Safe agent destruction and dlclose with
outstanding borrowed registrations are part of the upstream agreement above.

## Minimal native boundary and packaging

Use a separate, opt-in `orbite-nixl-ffi` crate with a `nixl-ffi` feature;
default Rust/Python builds neither export nor link it. Its C++ client lives in
NIXL, while its handles directly retain existing Rust owners. This is a real
packaging/language boundary, not restoration of `libtent` or the 33 TENT calls.
The following operation groups bound the proposed ABI; finalize signatures
with upstream rather than freeze speculative general-purpose APIs here.

| C operation group | Minimum data and existing owner |
| --- | --- |
| ABI information; engine create/close/release | ABI/record sizes, feature bits, explicit immutable OrbiTE JSON and engine handle; close returns outcome plus drain state. |
| Register/retire memory; optional shared allocation | Pointer, extent, kind, device, permissions and actual owner lease when available; generation-checked registry handle. Shared allocation reuses `allocate_shm_memory`, never copies caller memory. |
| Export/import peer and range metadata; retire peer | Caller-sized byte buffers, endpoint/epoch and registration identity; existing segment/registration leases. |
| Prepare/post/query/release execution | Bounded descriptor arrays, operation, deadline and optional notification; prepared owner references and existing `Batch`. Query returns logical code, transferred prefix, effect and physical drain separately. |
| Send/drain notifications | Length-delimited binary message, peer handle, caller-sized output buffers; existing charged queues. |
| Open/query/close local file (storage profile only) | Validated path/mode or duplicated fd, checked offsets; existing file Entry and close/drain. |

Opaque generation-checked handles, fixed-width `repr(C)` records with size and
ABI fields, explicit lengths and caller-owned output storage avoid Rust/STL
layout coupling and allocator mismatches. Generate `include/orbite/nixl.h`
from the Rust declarations with a build-only header generator; CI checks the
generated header, LP64 sizes/offsets and a minimal C consumer. Pin the bridge
to the exact core release if it uses private receipt helpers. Shared build
dependencies follow the normal minimum-version/lockfile policy and Rust 1.85.
This docs PR adds no header, ABI exports or build dependencies.

Every entry has a panic barrier and typed status. Unsafe conversion is scoped
and has `SAFETY` notes for pointer lifetime, bounds, alignment, aliasing and
callback ownership. Invoke foreign callbacks outside native locks. Reject
stale handles, oversized arrays, unknown record versions and overflow before
admission. Bound wrapper objects and metadata bytes explicitly against engine
registration, peer and batch limits; quarantine continues to consume those
budgets. Do not add an uncharged C++ pending-request list.

Install `liborbite_nixl.so`, its generated header and `orbite-nixl.pc`.
Meson discovers that dependency and builds the in-tree plugin using NIXL's
C++20/STL-only plugin interface. No Mooncake library or Python interpreter is
linked. Native CUDA/RDMA/GDS/fabric dependencies retain their current opt-in
loading rules. NIXL's plugin API version (currently 3), the bridge ABI version
and OrbiTE's wire policy are separate checks. Reject an ABI-major/layout
mismatch before engine construction. Additive records negotiate size/features;
breaking changes require a new ABI/SONAME and the applicable OrbiTE release
bump. Before the first stable release deploy matching plugin/core builds on
both peers under the [existing wire policy](tent-parity.md#wire-and-consumer-boundaries).
No new wire layout is frozen by this proposal.

Internal work needed for this boundary is limited but real: per-registration
retirement receipts (unregister alone is insufficient), nonblocking observation
of existing close phases, and fd adoption for the storage profile. Put each
with its current registry/segment/file owner and test it there. Do not implement
a second executor to hide a missing receipt.

## Acceptance controls

All rows below are **planned**, not new test evidence. Use NIXL GoogleTests
under its current `test/gtest/plugins/` layout, a small installed C bridge
consumer, and existing OrbiTE shims for injected native errors. Test the public
NIXL agent as well as direct backend methods so ignored return codes cannot
escape detection. Every byte test checks destination guards too.

| Control | Negative case and expected owner/charge state |
| --- | --- |
| N01: DRAM TCP/SHM | Two real processes, READ/WRITE, fragmented lists and merged aliases. Ordinary malloc cannot select SHM; an actual shared allocation can. Bytes and guards agree, accounting returns to baseline only after drain. |
| N02: active request release/cancel | Hold a real response after partial WRITE; release returns negative, check cannot report success, backing/peer/charges stay. Queued cancel releases only after proving nothing was posted. Unblocking produces exactly one retirement receipt. |
| N03: terminal error/destructor | Inject timeout, short completion and unknown WRITE effect; let NIXL delete its terminal wrapper. Existing deferred owners survive, no replay occurs, and metadata release cannot free the allocation prematurely. |
| N04: deregister/rollback | Deregister with local and remote borrowers; fail a later member of multi-registration. NIXL must propagate pending/failed cleanup and preserve a retirement token. Its current ignored-return path must fail this control. |
| N05: metadata unload/restart | Unload/invalidate during an admitted copy. New posts fail, admitted owners stay; a new epoch works, the old epoch cannot repost, and another peer is unaffected. |
| N06: shutdown and failed destroy | Inject failed QP/MR, event/import, file or library cleanup. Drain timeout is not success; allocations, callback code, native resources and charges remain retained. Repeated observation makes no second destroy call. |
| N07: prepare/repost and bounds | No payload during prep; pending CQE/producer readiness cannot block post/check/release. Active/failed/canceled handles cannot repost. A drained success may execute once more with fresh accounting. Overflow, budget exhaustion and mixed authorities fail before posting. |
| N08: notifications | Binary payload, self notification, stale sender epoch, oversized message and one unreachable target. Exactly-once logical delivery; transfer success requires its requested notification outcome. No WRITE replay on notification error. |
| N09: local files | Real io_uring ext4/tmpfs, both file-list positions and READ/WRITE, fragments, EOF inside a fragment, fd reuse and close during I/O. Copy back only the completed prefix. Remote FILE and unsupported modes fail before I/O. |
| N10: bridge/loader | Missing library/feature, ABI/size mismatch, stale handle, panic and C++ factory failure; no allocation/admission and no exception crossing ABI. Default core build has no plugin dependency. |

GitHub-hosted Linux runs N01–N05, N07–N10 for the CPU routes with packaged
OrbiTE, pinned NIXL, stable and Rust 1.85. Require io_uring explicitly for N09;
repeat timing-sensitive controls 50 times. Only those actual CPU/TCP/SHM/file
profiles may become CI-qualified after passing. CUDA/verbs/cuFile shims for
N02–N07 establish ownership behavior, not native qualification.

The {ref}`plugin verification queue <queue-nixl-plugin>` reserves
installed VRAM GDR/NVLink and FILE GDS cases, plus MNNVL if advertised. Plugin
gate drivers and exact commands must be written with the implementation; none
exist or run in this docs step. Require route evidence, full bytes/guards and
the same failure/retention controls on locked hardware. Existing native gates
are prerequisites, not evidence for a NIXL wrapper. `nixlbench` comparisons
against UCX and Mooncake wait for that hardware pass and use matched versions,
sizes, batch shapes, concurrency, warmup and byte validation.

## Upstream design issue draft — not posted

**Title:** Proposal: an OrbiTE backend with explicit transfer retirement

**Motivation.** OrbiTE is a Rust CPU/NVIDIA transfer engine for OrbitKV cache
tiering and cross-node reuse. We propose an in-tree `src/plugins/orbite`
backend so existing NIXL consumers can select it without custom inference
engine connectors. OrbiTE follows Mooncake TENT's design but has independent
API, configuration and wire formats. This is a design request before a PR.

**Scope.** A thin C++20/STL plugin over an opt-in native C bridge to existing
Rust owners. Start with DRAM TCP and genuine SHM mappings, then validated CUDA
registrations and their existing GDR/NVLink paths. Propose a separate local-only
`ORBITE_FILE` entry point for io_uring/GDS rather than remote FILE metadata.
No metastores, collectives, non-NVIDIA SDKs or plugin-specific scheduler.

**Lifetime contract needing agreement.** Logical failure, cancellation and
request deletion do not prove physical completion. At reviewed main
`540fae3a`, active release errors are propagated as `REPOST_ACTIVE`, terminal
request destructors ignore release errors, and registration removal ignores
deregistration errors. We need a pollable retirement path and error-preserving
registration rollback/destruction so callers know when a borrowed allocation
can be reused. Failed native cleanup retains its owners; an uncertain WRITE
is never replayed. Remote metadata invalidation must fence new posts while
the old native identity remains owned through drain. The implementation must
not depend on a consumer treating an ignored error as a release receipt.

**Questions for maintainers.** Is this lifetime contract acceptable, including
the core error-propagation changes and safe plugin unload? Is a local-only
storage entry point the preferred FILE integration? Are the proposed native
package discovery and CPU GoogleTest profiles suitable for an in-tree plugin?
Which existing request/registration retirement mechanism should we extend?

**Validation and rollout.** Real two-process DRAM TCP/SHM tests on hosted CI,
installed header/library tests, forced release/unload/deregistration races,
no-progress/no-replay controls and byte guards precede any qualification.
GPU/RDMA/GDS tests remain unqualified until installed runs; nixlbench UCX and
Mooncake comparisons wait for hardware. A later P/D design must map decode-page
abort safety, descriptor/notification lifetimes and joint P/D/cache arbitration,
then measure TTFT/ITL after GDR/NVLink qualification. This proposal alone makes
no P/D performance claim.

**Contribution process.** Request design agreement before a significant PR,
follow the project's tests and public STL boundary, and use the contributor's
own DCO sign-off. No issue, fork, upstream PR or package has been created by
this design work. The maintainer decides when to submit this draft.

[nixl]: https://github.com/ai-dynamo/nixl/tree/540fae3a13eaa2a8cfed2186fc0527ee0da8a379
[guide]: https://github.com/ai-dynamo/nixl/blob/540fae3a13eaa2a8cfed2186fc0527ee0da8a379/docs/BackendGuide.md
[backend]: https://github.com/ai-dynamo/nixl/blob/540fae3a13eaa2a8cfed2186fc0527ee0da8a379/src/api/cpp/backend/backend_engine.h
[plugin]: https://github.com/ai-dynamo/nixl/blob/540fae3a13eaa2a8cfed2186fc0527ee0da8a379/src/api/cpp/backend/backend_plugin.h
[mooncake]: https://github.com/ai-dynamo/nixl/tree/540fae3a13eaa2a8cfed2186fc0527ee0da8a379/src/plugins/mooncake
[contributing]: https://github.com/ai-dynamo/nixl/blob/540fae3a13eaa2a8cfed2186fc0527ee0da8a379/CONTRIBUTING.md
[files]: https://github.com/ai-dynamo/nixl/blob/540fae3a13eaa2a8cfed2186fc0527ee0da8a379/src/utils/file/README.md
[agent]: https://github.com/ai-dynamo/nixl/blob/540fae3a13eaa2a8cfed2186fc0527ee0da8a379/src/core/nixl_agent.cpp#L1570-L1619
[request]: https://github.com/ai-dynamo/nixl/blob/540fae3a13eaa2a8cfed2186fc0527ee0da8a379/src/core/transfer_request.h#L69-L73
[memory-section]: https://github.com/ai-dynamo/nixl/blob/540fae3a13eaa2a8cfed2186fc0527ee0da8a379/src/infra/nixl_memory_section.cpp#L383-L425
