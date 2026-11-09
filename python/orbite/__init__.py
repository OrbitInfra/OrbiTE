"""OrbiTE: retained transfer owners and explicit peer epochs.

The Python interface mirrors the Rust API. A timeout, cancellation or object
finalizer is never permission to release memory still used by native work.
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
    READ = 0
    WRITE = 1


class Priority(_IntEnum):
    HIGH = 0
    MEDIUM = 1
    LOW = 2


class Permission(_IntEnum):
    LOCAL_ONLY = 0
    REMOTE_READ = 1
    REMOTE_READ_WRITE = 2


class IntentType(_IntEnum):
    UNSPEC = 0
    FOREGROUND_GET = 1
    BACKGROUND_PREFETCH = 2
    MIGRATION = 3
    CHECKPOINT = 4
    WEIGHT_LOADING = 5
    STAGING_INTERNAL = 6


class TaskState(str, _Enum):
    WAITING = "waiting"
    RUNNING = "running"
    COMPLETED = "completed"
    FAILED = "failed"
    CANCELLED = "cancelled"
    SKIPPED = "skipped"


@_dataclass(slots=True)
class TransferRequest:
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
    name: str
    message: str


@_dataclass(slots=True)
class MemoryOptions:
    location: str = "*"
    permission: Permission = Permission.REMOTE_READ_WRITE
    transport: Transport = Transport.UNSPEC
    shm_path: str = ""
    shm_offset: int = 0
    internal: bool = False


@_dataclass(frozen=True, slots=True)
class TransferFailure:
    message: str
    remote_effect: str  # "none" or "unknown"; observational, never replay permission.


@_dataclass(frozen=True, slots=True)
class TaskStatus:
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
        self._native = _native._Config.from_json(_json.dumps(settings))

    @classmethod
    def from_json(cls, text):
        obj = cls.__new__(cls)
        obj._native = _native._Config.from_json(text)
        return obj

    @classmethod
    def from_file(cls, path):
        obj = cls.__new__(cls)
        obj._native = _native._Config.from_file(str(path))
        return obj

    @classmethod
    def from_env(cls):
        obj = cls.__new__(cls)
        obj._native = _native._Config.from_env()
        return obj


class _Owner:
    def __enter__(self):
        return self

    def __exit__(self, *_):
        self.close()


class Memory(_Owner):
    """Engine allocation. Dropping it defers free while native loans exist."""
    def __init__(self, engine, address, length):
        self._engine, self.address, self.length = engine, address, length
        self._closed = False

    def close(self):
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
    """Registration retaining its exporter through physical drain, including aliases."""
    def __init__(self, engine, address, length):
        self._engine, self.address, self.length = engine, address, length
        self._closed = False

    def close(self):
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
    """Requester-owned descriptor for an explicitly supplied endpoint and epoch."""
    def __init__(self, engine, handle, endpoint, epoch):
        self._engine, self._handle = engine, handle
        self.endpoint, self.epoch = endpoint, epoch
        self._closed = False

    def info(self):
        return self._engine._native.segment_info(self._handle)

    def close(self):
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
    """Local file owner. Close fences admissions and drains through the native owner."""


class Batch(_Owner):
    """Bounded batch; abandoning it transfers outstanding work to deferred retirement."""
    def __init__(self, engine, handle):
        self._engine, self._handle, self._closed = engine, handle, False

    def submit(self, operation, target, requests, notification=None):
        if target._engine is not self._engine:
            raise InvalidRangeError("segment belongs to another engine")
        self._engine._native.submit(self._handle, operation, target._handle, requests, notification)

    def tasks(self):
        return _tasks(self._engine._native.tasks(self._handle))

    def progress(self):
        return _tasks(self._engine._native.progress_batch(self._handle))

    def wait(self, timeout):
        """Wait for logical results and physical drain; seconds. Timeout keeps owners."""
        return _tasks(self._engine._native.wait(self._handle, timeout))

    def cancel(self, task):
        self._engine._native.cancel(self._handle, task)

    def close(self):
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
        self._native = _native._Engine(None if config is None else config._native)

    def endpoint(self):
        return self._native.endpoint()

    def source_epoch(self):
        return self._native.source_epoch()

    def stats(self):
        return _MappingProxyType(_json.loads(self._native.stats()))

    def local_topology(self):
        return _json.loads(self._native.local_topology())

    def allocate_memory(self, length, options=None):
        return Memory(self, self._native.allocate(length, options or MemoryOptions()), length)

    def register_memory(self, address, length, *, owner=None, options=None):
        self._native.register([address], [length], options or MemoryOptions(), [owner])
        return RegisteredMemory(self, address, length)

    def register_memory_ranges(self, ranges, *, owners=None, options=None):
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
        address, length = self._native.register_buffer(owner, options or MemoryOptions())
        return RegisteredMemory(self, address, length)

    def open_segment(self, endpoint, expected_epoch):
        return RemoteSegment(self, self._native.open_segment(endpoint, expected_epoch), endpoint, expected_epoch)

    def open_file_segment(self, path, permission=Permission.REMOTE_READ_WRITE):
        return FileSegment(self, self._native.open_file_segment("file://" + str(path), permission), str(path), None)

    def allocate_batch(self, capacity):
        return Batch(self, self._native.allocate_batch(capacity))

    def send_notification(self, target, notification):
        if target is not None and target._engine is not self:
            raise InvalidRangeError("segment belongs to another engine")
        self._native.send_notification(None if target is None else target._handle, notification)

    def receive_notifications(self):
        return [Notification(*row) for row in self._native.receive_notifications()]

    def close(self):
        self._native.close()
