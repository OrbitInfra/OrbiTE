# NIXL backend design

**Proposal only.** No plugin, C ABI or NIXL qualification exists yet. The target
is an in-tree backend in `ai-dynamo/nixl`, under `src/plugins/orbite`, rather
than a fork or a connector maintained inside each inference engine. OrbitKV's
upstream targets are vLLM and SGLang; the NIXL path would let vLLM
`NixlConnector`, SGLang's NIXL backend, Dynamo and LMCache P2P select OrbiTE.
This is an integration goal, not a claim that those consumers have tested it.

This review pins [NIXL main at `82b356ce`][nixl] (checked 2026-11-09, whose Meson
project version is `1.6.0`), following the maintainer's v1.5.2 survey. The
[South Bound guide][guide], [backend header][backend], [plugin entry points][plugin],
[Mooncake backend][mooncake] and [contribution rules][contributing] are the
contract reviewed here. Recheck that pin before implementation. OrbiTE's
design/oracle baseline remains Mooncake TENT v0.3.14; NIXL is neither a new
runtime dependency of the Rust crate nor a replacement for that baseline.

The [work order](completion-plan.md#current-focus) puts FABRIC removal,
dma-buf GPUDirect registration, local-copy SM budgeting and intent-aware Auto,
and the small-WRITE caller wait before plugin implementation. Design alignment
is complete. The upstream explicit-cancel API, the
outcome of the existing lifetime discussion and the hardware batch below are
prerequisites. The maintainer must approve the formal backend RFC. Nothing in
this document authorizes posting, commenting, forking, publishing or opening
an upstream PR.

## Related upstream work

States below were checked on **2026-11-09**. The reviewed main advanced from
`540fae3a` by one device-API test commit; the South Bound interface, guide and
three core lifetime behaviors below are unchanged. These are existing upstream
discussions and proposals, not new findings or a parallel cancellation design.

| Upstream item | State on 2026-11-09 | Consequence for this design |
| --- | --- | --- |
| [#1958: asynchronous request cleanup](https://github.com/ai-dynamo/nixl/issues/1958) | Open; `cancellation` label, filed by lluki, assigned to ColinNV | Follow NVIDIA's cancellation work. lluki favors keeping the request handle alive outside shutdown. A public comment mentions an internal design document; its contents were not reviewed here. |
| [#1758: deregistration results](https://github.com/ai-dynamo/nixl/issues/1758), [#2296: propagation draft](https://github.com/ai-dynamo/nixl/pull/2296) | #1758 closed; #2296 closed, unmerged | rakhmets expects release methods to warn and return void, with current `deregisterMem` always returning `NIXL_SUCCESS`. lluki's outstanding-transfer question remains relevant. Fence and defer internally; do not require propagation of deregistration errors. |
| [#2209: explicit cancel](https://github.com/ai-dynamo/nixl/pull/2209), [#2244: failed-release polling](https://github.com/ai-dynamo/nixl/pull/2244) | Both closed, unmerged | Target a separate cancel method; dropping a handle becomes terminal. Do not build clients that cancel by releasing. |
| [#2540: UCX active-handle retention](https://github.com/ai-dynamo/nixl/pull/2540) | Open | lluki's 2026-11-06 comment reiterates explicit cancel and opposes creating dependencies on implicit cancellation. Active release refusal is a safeguard, not the new cancel API. |
| [#2347: invalidated transfers and UCCL connections](https://github.com/ai-dynamo/nixl/pull/2347) | Open | Coordinate invalidation polling and connection retention with this work; fencing stale posts must not hide progress of admitted requests. |
| [#2077: POSIX deferred free](https://github.com/ai-dynamo/nixl/pull/2077), [#1955: active-handle use-after-free](https://github.com/ai-dynamo/nixl/issues/1955) | #2077 closed, unmerged; #1955 closed | Borrow the reference-counted lifetime principle, not #2077's success-before-drain release semantics. #1955's revised contract keeps a handle with outstanding work alive. |
| [#1792: duplicate-descriptor double free](https://github.com/ai-dynamo/nixl/issues/1792) | Open | Distinct registration aliases need distinct references; duplicate deregistration must not destroy one native owner twice. |
| [#2254: GPUNetIO release leak](https://github.com/ai-dynamo/nixl/issues/2254) | Open | Retirement must account for every operation of this request, never another request's completion or a shared ring position. |
| [#2082: Mooncake TENT mode](https://github.com/ai-dynamo/nixl/pull/2082), [#2083: its RFC](https://github.com/ai-dynamo/nixl/issues/2083) | Both open; authored by xiaodouzi666 | A working plugin and H20/RoCE runs are already reported. Its deferred-release engine list is relevant prior work; compare ownership contracts and results rather than duplicate the proposal. |

### Why an OrbiTE backend alongside Mooncake TENT mode?

TENT mode already has a working plugin and recorded H20/RoCE runs in the open
work above; OrbiTE has neither a NIXL plugin nor installed NIXL results on its
current artifact. Those upstream reports are not OrbiTE qualification.
OrbiTE offers an independent Rust implementation without the Mooncake C++
runtime, separate logical-outcome and physical-retirement receipts across
RDMA, CUDA, file and GDS paths, inference-intent priority, and one ownership
model for HBM, DRAM, NVMe and peers. These are reasons to evaluate another
backend, not performance claims or claims that TENT mode lacks these goals.
Intent propagation through NIXL still needs the request mapping below.

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
| `DRAM_SEG` | Retained `HostMemory` and registry ranges. TCP; opt-in SHM for actual shared mappings on the same boot identity; RDMA when installed. HP-TCP remains eligible only through their existing configured policies. |
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
| `deregisterMem` | Fence new posts immediately and drop the lookup reference. Defer native deregistration until the last borrower retires; failed cleanup retains the owner and charge internally with diagnostics. The expected `NIXL_SUCCESS` is not an allocation-release receipt. |
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
| `releaseReqH` | Release only after retirement; do not initiate cancellation. An active request returns a **negative** status and keeps every owner and the handle. `IN_PROG` is not safe here. A dropped wrapper cannot free unresolved native owners. |
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

The planned explicit cancel method is absent from the pinned header. When it
lands, map it to existing queued cancellation and running-I/O drain, then keep
polling until retirement. Do not invent its signature or emulate it through
`releaseReqH` in the meantime.

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
and `RemoteEffect`) and physical retirement receipt. **Every terminal
`checkXfer` status, including an error, must mean the backend can no longer
access this execution's local or remote buffers.** Logical failure with drain
pending stays `NIXL_IN_PROG`. Success additionally requires every public alias
and required notification to succeed. `Batch::free()` can defer retirement;
its success alone is **not** a receipt. Check source-side borrowers and the
native/staging/context owners too; a local status alone is insufficient.

| OrbiTE outcome | Proposed NIXL status and retention |
| --- | --- |
| Prepared, not posted | `NIXL_ERR_NOT_POSTED`; no payload effect. |
| Admitted/queued/running, or outcome known but drain pending | `NIXL_IN_PROG` from `checkXfer`; retain all owners. Release returns a negative error until safe. |
| All tasks and required notification succeeded, local and remote buffer access retired | `NIXL_SUCCESS` from `checkXfer`. Request release succeeds only after its remaining resource-retirement checks. |
| Queued cancellation, proven drained | `NIXL_ERR_CANCELED`; all merged aliases share the owner's result. Running I/O still drains. |
| Invalid range/config/type before posting | `NIXL_ERR_INVALID_PARAM`; unsupported capability → `NIXL_ERR_NOT_SUPPORTED`; permission failure → `NIXL_ERR_NOT_ALLOWED`. No admitted payload work. |
| Epoch/generation mismatch or removed metadata | `NIXL_ERR_MISMATCH` / `NIXL_ERR_NOT_FOUND` before posting or after drain; an admitted execution remains `IN_PROG` until retired. Never bind the old handle to a new peer. |
| Resource exhausted before posting | `NIXL_ERR_BACKEND` plus the bridge's typed detail; no unbounded overflow allocation. NIXL has no matching resource-exhausted status. |
| Provably unposted disconnect | `NIXL_ERR_REMOTE_DISCONNECT`, retaining the unposted/effect-known detail. A status code by itself never authorizes replay. |
| Short/failed I/O, timeout after admission, unknown WRITE effect | Stay `NIXL_IN_PROG` until physical retirement, then `NIXL_ERR_BACKEND` with the exact prefix/effect in the native result. Retirement does not make the WRITE's effect known. No plugin retry or cross-backend replay. |
| `Unquiesced`, including failed native cleanup | No safe terminal mapping is agreed in the current API. Retain owners, charges and code/library lifetime; never expose a sticky `NIXL_ERR_BACKEND` as a retirement receipt. The upstream question below must be settled before implementation. |

NIXL cannot encode OrbiTE's full error or an independent drain flag in one
status. Keep those details in the bridge result, report bounded diagnostic
counters, and do not promote an error to success to make a handle disappear.
A terminal status permits buffer reuse only because physical retirement has
already been proved; it never grants permission to replay an uncertain WRITE.

The pinned [BackendGuide transfer section][guide-transfer] still describes
abort-on-release, release errors until completion, and an error from
`checkXfer` after a failed abort. This proposal keeps active release refusal
but deliberately rejects the other two behaviors: use the agreed explicit
cancel direction in #2209/#2244/#2540, and keep `checkXfer` in progress while
any buffer access remains possible. Dropping a handle is terminal, not a
cancellation request. Clients must poll to retirement before release; an
incorrect early drop retains owners rather than freeing memory they may use.

### Open lifetime questions in the existing upstream work

The pinned core still has three behaviors covered by the related work above:

1. [`releaseXferReq`][agent] treats a negative active-release result as
   `NIXL_ERR_REPOST_ACTIVE` and retains the handle, but treats any nonnegative
   result as released. Therefore `releaseReqH` must never return `IN_PROG`
   while owners remain. A negative refusal preserves the in-progress handle
   for polling at this revision. Once status is terminal, the
   [`nixlXferReqH` destructor][request] ignores the release result.
2. [`nixlLocalSection::remDescList`][memory-section] ignores `deregisterMem`'s
   result, removes metadata and returns success. Registration rollback and
   destructor paths also ignore cleanup results. Follow #1758's expectation:
   fence new posts and defer native deregistration to the last borrower.
   Failed cleanup stays an internal `Unquiesced` owner with diagnostics, never
   a retried destroy. The reference-counted lifetime in the closed #2077
   proposal is useful here, without adopting success-before-retirement for
   transfer release.
3. [Remote invalidation][invalidation] removes the remote section and calls
   `disconnect`, but erases its backend mapping even when disconnect fails. The plugin must
   fence and retain identities independently of those C++ containers. NIXL's
   expired remote-section check rejects stale posts but also blocks polling of
   admitted work. That core error is not a backend retirement receipt.
   #2347 addresses that progress/connection-lifetime boundary; coordinate
   with it rather than introduce a competing mechanism.

**Prerequisites:** the upstream explicit-cancel API and the #1958 outcome must
resolve how a permanently unproven retirement is represented. A distinct
nonterminal status with continued ownership, or an explicit quarantine contract
that keeps borrowed allocations and code alive, are questions for upstream.
Neither is agreed, and no existing terminal status is claimed safe for
`Unquiesced`. Keeping `IN_PROG` while drain is pending is necessary but does not
by itself settle permanent failure, agent shutdown or library unload.
Propagation of deregistration errors is **not** an implementation prerequisite.

NIXL passes raw application pointers, not allocator owners. For borrowed
registration, the caller must keep the allocation and CUDA context alive until
physical retirement, including source-side borrowers; a successful
`deregisterMem` call only fences admission under the proposed deferred cleanup.
Retaining a C++ metadata object or a Rust `Arc` containing its address does
**not** retain that allocation.
Where an actual allocator lease is available, the bridge accepts retain/release
callbacks and releases it only on a receipt. For borrowed pointers, the NIXL
contract must make that obligation observable. How to do so with success-only
deregistration and agent destruction remains open under #1758/#2296. With
uncertain cleanup, retaining metadata alone cannot make borrowed memory safe;
do not claim support for freeing it after deregistration or destruction without
a physical receipt. Allocator leases help only when they own the actual backing.

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
outstanding borrowed registrations remain open questions under #1758/#2296
and the #1958 outcome, not a contract this backend can impose by itself.

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
| Prepare/post/cancel/query/release execution | Bounded descriptor arrays, operation, deadline and optional notification; prepared owner references and existing `Batch`. Cancel is separate from release, following the future upstream API. Query returns logical code, transferred prefix, effect and physical drain separately. |
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
linked. Native CUDA/RDMA/GDS dependencies retain their current opt-in
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
| N02: active request release/cancel | Hold a real response after partial WRITE. Release returns negative without invoking cancel; the handle remains pollable, check stays `IN_PROG`, and backing/peer/charges stay. Exercise the future explicit cancel separately: queued work proves nothing posted; running I/O drains. Unblocking produces exactly one retirement receipt. |
| N03: logical failure before retirement | Inject timeout, short completion and unknown WRITE effect while withholding one operation's receipt. No terminal `checkXfer` status until every local/remote access retires; another request's completion cannot retire this one. Early wrapper drop retains unresolved owners. No replay, and no later buffer writes after a terminal result. |
| N04: deregister/rollback/duplicates | Deregister with local and remote borrowers, duplicate descriptors, and a later multi-registration failure. Fence new posts immediately; NIXL may return success while native cleanup is deferred. Keep the actual allocation lease, registration and charge until the last borrower retires, and destroy each owner once. Borrowed-pointer tests require the upstream allocation-lifetime agreement; success alone must not authorize freeing. |
| N05: metadata unload/restart | Unload/invalidate during an admitted copy. New posts fail, admitted owners stay and remain pollable even after core metadata removal; a new epoch works, the old epoch cannot repost, and another peer is unaffected. Align with #2347. |
| N06: shutdown and failed destroy | Inject failed QP/MR, event/import, file or library cleanup. `Unquiesced` must not become terminal `ERR_BACKEND`; allocations, callback code, native resources and charges stay. Repeated observation makes no second destroy call. The accepted upstream quarantine/shutdown rule must prevent borrowed-memory reuse and dlclose; this control remains blocked until that rule exists. |
| N07: prepare/repost and bounds | No payload during prep; pending CQE/producer readiness cannot block post/check/release. Active/failed/canceled handles cannot repost. A drained success may execute once more with fresh accounting. Overflow, budget exhaustion and mixed authorities fail before posting. |
| N08: notifications | Binary payload, self notification, stale sender epoch, oversized message and one unreachable target. Exactly-once logical delivery; transfer success requires its requested notification outcome. No WRITE replay on notification error. |
| N09: local files | Real io_uring ext4/tmpfs, both file-list positions and READ/WRITE, fragments, EOF inside a fragment, fd reuse and close during I/O. Copy back only the completed prefix. Remote FILE and unsupported modes fail before I/O. |
| N10: bridge/loader | Missing library/feature, ABI/size mismatch, stale handle, panic and C++ factory failure; no allocation/admission and no exception crossing ABI. Default core build has no plugin dependency. |

After the upstream contracts are settled, GitHub-hosted Linux will run
N01–N05, N07–N10 for the CPU routes with packaged OrbiTE, pinned NIXL, stable
and Rust 1.85. Require io_uring explicitly for N09; repeat timing-sensitive
controls 50 times. Only those actual CPU/TCP/SHM/file
profiles may become CI-qualified after passing. CUDA/verbs/cuFile shims for
N02–N07 establish ownership behavior, not native qualification.

The {ref}`plugin verification queue <queue-nixl-plugin>` reserves
installed VRAM GDR/NVLink and FILE GDS cases, plus MNNVL if advertised. Plugin
gate drivers and exact commands must be written with the implementation; none
exist or run in this docs step. Require route evidence, full bytes/guards and
the same failure/retention controls on locked hardware. Before the formal
backend RFC, the hardware batch must produce installed GPU/RDMA results for
the current OrbiTE artifact and `nixlbench` UCX/Mooncake baselines. Native gates
do not qualify a wrapper that does not exist. After implementation, compare
the OrbiTE plugin on the same hardware with matched versions, sizes, batch
shapes, concurrency, warmup and byte validation. No plugin performance claim
precedes those measurements.

## Upstream design issue draft — v2, not posted

**Title:** Proposal: an OrbiTE backend following NIXL's explicit-cancel contract

**Hold before posting.** This formal backend RFC waits for the upstream
explicit-cancel API, the #1958 outcome, installed GPU/RDMA results on the
current OrbiTE artifact and the hardware batch's `nixlbench` UCX/Mooncake
baselines. Recheck upstream state then; the maintainer decides whether to post.

**Proposal.** An in-tree `src/plugins/orbite` backend for OrbiTE, an independent
Rust CPU/NVIDIA engine for KV movement. A small C++/STL adapter and opt-in C
bridge reuse its existing owners and inference-intent scheduler, without the
Mooncake runtime or a new coordinator. Start with DRAM TCP/SHM and validated
VRAM GDR/NVLink; ask whether a local-only `ORBITE_FILE` entry point fits storage.

**Existing work and questions.** Follow [#1958 (open, 2026-11-09)](https://github.com/ai-dynamo/nixl/issues/1958)
and the explicit-cancel direction reiterated in
[#2540 (open, 2026-11-09)](https://github.com/ai-dynamo/nixl/pull/2540), not a
parallel cancellation protocol. Every terminal check must mean physical
retirement; failed-but-draining work stays in progress. How should permanent
`Unquiesced` retention, borrowed allocations under success-only deregistration,
and agent/DSO shutdown be represented without a false receipt? Deregistration
fences new posts and defers cleanup internally. An uncertain WRITE never replays.

**Evidence and scope.** TENT mode already reports a working plugin and
H20/RoCE results; OrbiTE's distinction is its Rust dependency boundary and
physical-retirement owners across HBM, DRAM, NVMe and peers. Planned CPU
GoogleTests move real TCP/SHM bytes with lifetime and guard checks; GPU/file
routes require installed gates. No performance claim is made here. Attach
the hardware results and comparison parameters before requesting agreement.

Follow [NIXL's contribution process][contributing]: design agreement before a
significant PR, tests, public STL types and the contributor's own DCO sign-off.
This draft is not authorization to post or implement the plugin.

[nixl]: https://github.com/ai-dynamo/nixl/tree/82b356ce0a29b5b749d8b8720caceec6c844cd03
[guide]: https://github.com/ai-dynamo/nixl/blob/82b356ce0a29b5b749d8b8720caceec6c844cd03/docs/BackendGuide.md
[backend]: https://github.com/ai-dynamo/nixl/blob/82b356ce0a29b5b749d8b8720caceec6c844cd03/src/api/cpp/backend/backend_engine.h
[plugin]: https://github.com/ai-dynamo/nixl/blob/82b356ce0a29b5b749d8b8720caceec6c844cd03/src/api/cpp/backend/backend_plugin.h
[mooncake]: https://github.com/ai-dynamo/nixl/tree/82b356ce0a29b5b749d8b8720caceec6c844cd03/src/plugins/mooncake
[contributing]: https://github.com/ai-dynamo/nixl/blob/82b356ce0a29b5b749d8b8720caceec6c844cd03/CONTRIBUTING.md
[files]: https://github.com/ai-dynamo/nixl/blob/82b356ce0a29b5b749d8b8720caceec6c844cd03/src/utils/file/README.md
[agent]: https://github.com/ai-dynamo/nixl/blob/82b356ce0a29b5b749d8b8720caceec6c844cd03/src/core/nixl_agent.cpp#L1570-L1619
[request]: https://github.com/ai-dynamo/nixl/blob/82b356ce0a29b5b749d8b8720caceec6c844cd03/src/core/transfer_request.h#L69-L73
[memory-section]: https://github.com/ai-dynamo/nixl/blob/82b356ce0a29b5b749d8b8720caceec6c844cd03/src/infra/nixl_memory_section.cpp#L383-L425
[guide-transfer]: https://github.com/ai-dynamo/nixl/blob/82b356ce0a29b5b749d8b8720caceec6c844cd03/docs/BackendGuide.md#transfer-operations
[invalidation]: https://github.com/ai-dynamo/nixl/blob/82b356ce0a29b5b749d8b8720caceec6c844cd03/src/core/nixl_agent.cpp#L2025-L2048
