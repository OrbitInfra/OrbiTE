"""OrbiTE is a High Performance Transfer Engine for LLM Inference.

The Python interface mirrors the Rust API, including retained transfer owners
and explicit peer epochs. A timeout, cancellation or object finalizer is never
permission to release memory still used by native work.
"""
from dataclasses import dataclass as _dataclass
from enum import Enum as _Enum, IntEnum as _IntEnum
import json as _json
from types import MappingProxyType as _MappingProxyType
from . import _native

# One exception per Rust Error variant; ordinary Python type errors remain type errors.
for _name in dir(_native):
    if _name == "Error" or _name.endswith("Error"):
        globals()[_name] = getattr(_native, _name)


class Transport(_IntEnum):
    """Transport hint; UNSPEC selects by policy, explicit hints must be available.

    TCP and HP_TCP use sockets; SHM uses same-host mappings. RDMA, NVLINK and
    MNNVL require their native capabilities. IO_URING and GDS address local files.
    An enum member's presence does not imply the provider is built or installed.
    """
    UNSPEC = 0
    TCP = 1
    HP_TCP = 2
    SHM = 3
    RDMA = 4
    NVLINK = 6
    MNNVL = 7
    IO_URING = 8
    GDS = 9


class TransferOp(_IntEnum):
    """READ copies target to local memory; WRITE copies local memory to target."""
    READ = 0
    WRITE = 1


class Priority(_IntEnum):
    """HIGH, MEDIUM and LOW scheduling classes, in decreasing priority order."""
    HIGH = 0
    MEDIUM = 1
    LOW = 2


class Permission(_IntEnum):
    """LOCAL_ONLY forbids remote access; REMOTE_READ permits remote reads.

    REMOTE_READ_WRITE permits both directions. These are registration permissions,
    not authentication of a peer or isolation of a shared CUDA IPC allocation.
    """
    LOCAL_ONLY = 0
    REMOTE_READ = 1
    REMOTE_READ_WRITE = 2


class IntentType(_IntEnum):
    """Selector workload intent, independent of the READ/WRITE operation.

    UNSPEC leaves intent unspecified. FOREGROUND_GET, BACKGROUND_PREFETCH,
    MIGRATION, CHECKPOINT, WEIGHT_LOADING and STAGING_INTERNAL identify the
    caller's workload; a policy still determines whether a route is eligible.
    """
    UNSPEC = 0
    FOREGROUND_GET = 1
    BACKGROUND_PREFETCH = 2
    MIGRATION = 3
    CHECKPOINT = 4
    WEIGHT_LOADING = 5
    STAGING_INTERNAL = 6


class TaskState(str, _Enum):
    """Logical WAITING/RUNNING or terminal COMPLETED/FAILED/CANCELLED/SKIPPED state.

    A terminal state alone is not physical retirement; inspect locally_drained.
    """
    WAITING = "waiting"
    RUNNING = "running"
    COMPLETED = "completed"
    FAILED = "failed"
    CANCELLED = "cancelled"
    SKIPPED = "skipped"


@_dataclass(slots=True)
class TransferRequest:
    """One byte range, validated and retained when Batch.submit admits it.

    local_address is an address in a local registration; remote_address is a
    peer address or file offset. length is in bytes. transport defaults to
    UNSPEC, priority to HIGH, policy_name to the target's default policy, and
    intent to UNSPEC. deadline_ns is an absolute time.monotonic_ns() deadline;
    zero uses the engine's submission timeout. Constructing this record does
    not register memory, acquire a loan or prove the addresses valid.
    """
    local_address: int
    remote_address: int
    length: int
    transport: Transport = Transport.UNSPEC
    priority: Priority = Priority.HIGH
    policy_name: str | None = None
    deadline_ns: int = 0  # Absolute time.monotonic_ns(); zero means the submit timeout.
    intent: IntentType = IntentType.UNSPEC


@_dataclass(slots=True)
class Notification:
    """A name and message, each a string, subject to the engine's UTF-8 byte budget."""
    name: str
    message: str


@_dataclass(slots=True)
class MemoryOptions:
    """Allocation/registration options captured by the native owner.

    location is '*', 'cpu[:NUMA-node]' or 'cuda[:device-ordinal]'; '*' is the
    default. permission defaults to REMOTE_READ_WRITE, transport to UNSPEC.
    shm_path and byte shm_offset identify an explicit shared mapping (defaults:
    empty path and zero offset). internal defaults to False; True hides the
    registration from peer Describe replies, without authenticating callers.
    Unsupported combinations raise a typed Error before registration is exposed.
    """
    location: str = "*"
    permission: Permission = Permission.REMOTE_READ_WRITE
    transport: Transport = Transport.UNSPEC
    shm_path: str = ""
    shm_offset: int = 0
    internal: bool = False


@_dataclass(frozen=True, slots=True)
class TransferFailure:
    """Task failure message and remote_effect ('none' or 'unknown').

    remote_effect describes possible remote mutation; neither value is a general
    retry authorization. An uncertain WRITE must never be replayed.
    """
    message: str
    remote_effect: str  # "none" or "unknown"; observational, never replay permission.


@_dataclass(frozen=True, slots=True)
class TaskStatus:
    """One logical task's immutable observation, not a fresh transfer authority.

    state is its TaskState; transferred_bytes is its reported byte prefix.
    physical_owner identifies the shared physical task for merged aliases.
    transport names the selected route and failover_count counts fallbacks.
    cancel_requested records the request, while locally_drained separately
    reports physical retirement. failure is a TransferFailure or None.
    """
    state: TaskState
    transferred_bytes: int
    physical_owner: int
    transport: str
    failover_count: int
    cancel_requested: bool
    locally_drained: bool
    failure: TransferFailure | None


def _tasks(text):
    rows = _json.loads(text)
    return [TaskStatus(**{**r, "state": TaskState(r["state"]),
                         "failure": TransferFailure(**r["failure"]) if r["failure"] else None})
            for r in rows]


class EngineConfig:
    """Immutable native snapshot. Explicit JSON/file input ignores ORBITE_CONF."""
    __slots__ = ("_native",)

    def __init__(self, **settings):
        """Validate native configuration fields; defaults fill omitted settings.

        ConfigError rejects document/field parsing errors. InvalidConfigError
        and subsystem-specific errors reject semantic constraints and bounds.
        Explicit settings do not consult ORBITE_CONF or start an engine.
        """
        self._native = _native._Config.from_json(_json.dumps(settings))

    @classmethod
    def from_json(cls, text):
        """Parse a bounded JSON string, rejecting unknown/duplicate keys and bad types."""
        obj = cls.__new__(cls)
        obj._native = _native._Config.from_json(text)
        return obj

    @classmethod
    def from_file(cls, path):
        """Read bounded JSON; ConfigError reports file/parsing failures.

        Semantic constraints can raise InvalidConfigError or subsystem errors.
        """
        obj = cls.__new__(cls)
        obj._native = _native._Config.from_file(str(path))
        return obj

    @classmethod
    def from_env(cls):
        """Capture ORBITE_CONF once (inline JSON or a path), or native defaults if unset."""
        obj = cls.__new__(cls)
        obj._native = _native._Config.from_env()
        return obj


class _Owner:
    def __enter__(self):
        """Return this owner; entering does not allocate or reopen native resources."""
        return self

    def __exit__(self, *_):
        """Close explicitly without suppressing exceptions; cleanup failure may replace a body error."""
        self.close()


class Memory(_Owner):
    """Engine allocation with address and byte length, returned by allocate_memory.

    Dropping it defers free while native loans exist. Keep the engine alive and
    register the range before submitting transfers; allocation is not registration.
    """
    def __init__(self, engine, address, length):
        """Wrap an engine allocation receipt; consumers obtain it from allocate_memory."""
        self._engine, self.address, self.length = engine, address, length
        self._closed = False

    def close(self):
        """Free an exclusively held allocation; successful repeated close is harmless.

        ResourceExhaustedError means registrations/loans still hold it.
        UnquiescedError retains native backing and its charge after failed cleanup.
        """
        if not self._closed:
            self._engine._native.free(self.address)
            self._closed = True

    def __del__(self):
        if not getattr(self, "_closed", True):
            try:
                self._engine._native.abandon_memory(self.address)
            except Error:
                pass  # Native engine retains uncertain owners and their charges.
            self._closed = True


class RegisteredMemory(_Owner):
    """Registration with address and byte length, retaining its exporter through drain.

    Aliases share the native allocation lifetime. Obtain this guard from an
    engine registration method; dropping it is not a physical release receipt.
    """
    def __init__(self, engine, address, length):
        """Wrap a successful native registration receipt for this engine and range."""
        self._engine, self.address, self.length = engine, address, length
        self._closed = False

    def close(self):
        """Unregister this range; a successful repeated close does nothing.

        Busy backing can raise ResourceExhaustedError. Close related aliases with
        unregister_memory_ranges; uncertain cleanup keeps exporters and charges.
        """
        if not self._closed:
            self._engine._native.unregister([self.address], [self.length])
            self._closed = True

    def __del__(self):
        if not getattr(self, "_closed", True):
            try:
                self.close()
            except Error:
                pass  # The charged registration remains in its engine until drain/close.


class RemoteSegment(_Owner):
    """Requester-owned descriptor for an authoritative endpoint and source epoch.

    endpoint and epoch are discovery inputs supplied by the consumer. This handle
    cannot authorize access after the source incarnation changes.
    """
    def __init__(self, engine, handle, endpoint, epoch):
        """Wrap a native descriptor receipt; obtain peer handles with open_segment."""
        self._engine, self._handle = engine, handle
        self.endpoint, self.epoch = endpoint, epoch
        self._closed = False

    def info(self):
        """Return (is_file, [(base_or_offset, byte_length, location), ...]).

        Peer rows describe the opened snapshot; file rows inspect the local extent.
        This observation neither registers bytes nor grants new peer authority.
        """
        return self._engine._native.segment_info(self._handle)

    def close(self):
        """Release this descriptor handle; successful repeated close does nothing.

        Peer close does not retire the engine's endpoint/epoch identity and the
        same peer may be reopened. For FileSegment, close fences and drains I/O;
        a drain/cleanup error leaves its native owner retained.
        """
        if not self._closed:
            self._engine._native.close_segment(self._handle)
            self._closed = True

    def __del__(self):
        if not getattr(self, "_closed", True):
            try:
                self.close()
            except Error:
                pass


class FileSegment(RemoteSegment):
    """Local file owner returned by open_file_segment; endpoint is its path, epoch None.

    Requests use file offsets, not peer virtual addresses. Close fences admissions
    and drains through the native owner, retaining the fd on uncertain cleanup.
    """


class Batch(_Owner):
    """Bounded batch; abandoning it transfers outstanding work to deferred retirement."""
    def __init__(self, engine, handle):
        """Wrap a batch receipt returned by the engine's allocate_batch method."""
        self._engine, self._handle, self._closed = engine, handle, False

    def submit(self, operation, target, requests, notification=None):
        """Append requests using READ/WRITE and a segment owned by this engine.

        Each request is validated before admission; the optional Notification
        accompanies this submission. Capacity, route, range and permission errors
        raise typed Error subclasses. A failed submit is not proof of retirement
        for work already admitted; observe tasks and drain before reusing bytes.
        """
        if target._engine is not self._engine:
            raise InvalidRangeError("segment belongs to another engine")
        self._engine._native.submit(self._handle, operation, target._handle, requests, notification)

    def tasks(self):
        """Return TaskStatus observations without explicitly driving batch progress."""
        return _tasks(self._engine._native.tasks(self._handle))

    def progress(self):
        """Advance one bounded native progress step and return TaskStatus observations."""
        return _tasks(self._engine._native.progress_batch(self._handle))

    def wait(self, timeout):
        """Wait up to timeout seconds for terminal tasks and physical drain.

        Return TaskStatus records, including failed/cancelled tasks; inspect each
        failure. TimeoutError preserves pending owners. UnquiescedError reports
        uncertain native retirement. timeout must be finite and nonnegative.
        Blocking native waits release the GIL so other threads can make progress.
        """
        return _tasks(self._engine._native.wait(self._handle, timeout))

    def cancel(self, task):
        """Request cancellation by zero-based task index, or raise InvalidTaskError.

        Acceptance is not a drain receipt; running native work keeps its owners.
        """
        self._engine._native.cancel(self._handle, task)

    def close(self):
        """Free a physically drained batch; raises BatchBusyError while work owns it.

        Successful repeated close is harmless. Dropping a busy guard instead
        transfers pending work to the engine's deferred retirement owner.
        """
        if not self._closed:
            self._engine._native.free_batch(self._handle)
            self._closed = True

    def __del__(self):
        if not getattr(self, "_closed", True):
            try:
                self._engine._native.abandon_batch(self._handle)
            except Error:
                pass
            self._closed = True


class TransferEngine(_Owner):
    """Native engine. None loads ORBITE_CONF once; EngineConfig bypasses the environment."""
    def __init__(self, config=None):
        """Start native owners using EngineConfig, or capture ORBITE_CONF when None.

        Configuration, binding and bounded-resource failures raise typed Error
        subclasses. Keep the engine alive through all allocation/transfer guards.
        """
        self._native = _native._Engine(None if config is None else config._native)

    def endpoint(self):
        """Return the bound control endpoint as a string for consumer-owned discovery."""
        return self._native.endpoint()

    def source_epoch(self):
        """Return this source incarnation's opaque epoch string; publish it with endpoint."""
        return self._native.source_epoch()

    def stats(self):
        """Return a read-only mapping of engine counters and retained resource charges.

        Counters are sampled observations, not a drain receipt. Native retirement
        sweeping can raise UnquiescedError while preserving unresolved owners.
        """
        return _MappingProxyType(_json.loads(self._native.stats()))

    def local_topology(self):
        """Return nic_list and mem_list observations; availability is not qualification."""
        return _json.loads(self._native.local_topology())

    def allocate_memory(self, length, options=None):
        """Allocate length bytes with MemoryOptions defaults, returning a Memory guard.

        This does not register the allocation. Invalid sizes, unavailable locations
        and exhausted budgets raise typed errors; backing survives native loans.
        """
        return Memory(self, self._native.allocate(length, options or MemoryOptions()), length)

    def register_memory(self, address, length, *, owner=None, options=None):
        """Register a byte range and return its RegisteredMemory guard.

        Foreign addresses require a retained writable exporter in owner. Bare
        foreign integers are rejected; engine allocations need no foreign owner.
        Bounds, permission and provider failures raise typed Error subclasses.
        """
        self._native.register([address], [length], options or MemoryOptions(), [owner])
        return RegisteredMemory(self, address, length)

    def register_memory_ranges(self, ranges, *, owners=None, options=None):
        """Validate a bounded sequence of (address, byte_length) pairs, then register it.

        owners, when supplied, must have one exporter or None per range. All entries
        use the supplied MemoryOptions (or defaults). Return registration guards;
        invalid lists, foreign pointers or capacity failures raise typed errors.
        """
        if len(ranges) > self._native.registration_limit():
            raise ResourceExhaustedError("registration list exceeds engine capacity")
        addresses, lengths = zip(*ranges) if ranges else ([], [])
        self._native.register(addresses, lengths, options or MemoryOptions(), owners)
        return [RegisteredMemory(self, a, n) for a, n in zip(addresses, lengths)]

    def unregister_memory_ranges(self, registrations):
        """Close related registrations together, including aliases of one CUDA allocation."""
        if len(registrations) > self._native.registration_limit():
            raise ResourceExhaustedError("registration list exceeds engine capacity")
        if any(r._engine is not self or r._closed for r in registrations):
            raise InvalidRangeError("registration is closed or belongs to another engine")
        self._native.unregister([r.address for r in registrations], [r.length for r in registrations])
        for registration in registrations:
            registration._closed = True

    def register_buffer(self, owner, options=None):
        """Register and retain an entire writable contiguous Python or CUDA exporter.

        CUDA Array Interface/DLPack exports require native CUDA support and checked
        allocation/device bounds; CPU wheels reject them before requesting a capsule.
        Return a RegisteredMemory guard; readonly/noncontiguous exports are rejected.
        """
        address, length = self._native.register_buffer(owner, options or MemoryOptions())
        return RegisteredMemory(self, address, length)

    def open_segment(self, endpoint, expected_epoch):
        """Open a peer descriptor using its authoritative endpoint and epoch strings.

        The engine's handler timeout bounds the handshake. EpochMismatchError
        rejects stale identity; OrbiTE supplies no discovery or membership service.
        """
        return RemoteSegment(self, self._native.open_segment(endpoint, expected_epoch), endpoint, expected_epoch)

    def open_file_segment(self, path, permission=Permission.REMOTE_READ_WRITE):
        """Open a local filesystem path with the requested permission; return FileSegment.

        The path has no file:// prefix. Transfers need a built/enabled file provider;
        file opening alone does not qualify io_uring or GDS execution.
        """
        return FileSegment(self, self._native.open_file_segment("file://" + str(path), permission), str(path), None)

    def allocate_batch(self, capacity):
        """Reserve a bounded request capacity and return a Batch guard.

        Invalid capacities or exhausted engine budgets raise typed errors.
        """
        return Batch(self, self._native.allocate_batch(capacity))

    def send_notification(self, target, notification):
        """Send Notification to a peer owned by this engine, or locally when target is None.

        Queue/UTF-8 byte budgets apply. File targets are unsupported. Notification
        delivery is separate from payload completion and never proves its drain.
        """
        if target is not None and target._engine is not self:
            raise InvalidRangeError("segment belongs to another engine")
        self._native.send_notification(None if target is None else target._handle, notification)

    def receive_notifications(self):
        """Drain the bounded local notification queue into a list of Notification records."""
        return [Notification(*row) for row in self._native.receive_notifications()]

    def close(self):
        """Close native admissions and drain using the configured handler timeout.

        The result is cached: repeated calls return the same success or error.
        Failed/uncertain cleanup retains the native slot, backing and charges;
        a later call never retries a possibly half-completed native destroy.
        """
        self._native.close()
