"""Replay the shared valid-input CPU trace through an installed OrbiTE wheel."""

from __future__ import annotations

import sys
import weakref
from dataclasses import dataclass, field
from errno import ECONNRESET
from os import strerror
from pathlib import Path

import orbite

BULK_BYTES = 64 << 20


class Destination(bytearray):
    """A weak reference can observe the backing without keeping it alive."""


@dataclass
class Buffer:
    backing: bytearray
    registrations: list[orbite.RegisteredMemory] = field(default_factory=list)

    @property
    def address(self) -> int:
        return self.registrations[0].address

    def end(self, offset: int, length: int) -> int:
        end = offset + length
        if offset < 0 or length <= 0 or end > len(self.backing):
            raise ValueError("trace range outside buffer")
        return end

    def unregister(self) -> None:
        self.registrations[0].close()
        del self.registrations[0]

    def check_export(self) -> None:
        if self.registrations:
            try:
                self.backing.append(0)
            except BufferError:
                return
            raise RuntimeError("live registration aliases lost the exporter")
        self.backing.append(0)
        self.backing.pop()


def observe(*fields: object) -> None:
    print("OBS", *fields, sep="\t", flush=True)


def text(field: str) -> str:
    return "" if field == "-" else field


def finish_batch(batch: orbite.Batch, label: str, count: int) -> None:
    tasks = batch.wait(5)
    if len(tasks) != count or any(
        task.state != orbite.TaskState.COMPLETED
        or not task.locally_drained
        or task.failure is not None
        or task.physical_owner != index
        for index, task in enumerate(tasks)
    ):
        raise RuntimeError(f"transfer {label} failed: {tasks}")
    for _ in range(2):
        if batch.tasks() != tasks:
            raise RuntimeError(f"terminal status changed for {label}")
    observe(
        "RESULT",
        label,
        "completed",
        len(tasks),
        sum(task.transferred_bytes for task in tasks),
    )


def check_released(name: str, engine: orbite.TransferEngine) -> None:
    stats = engine.stats()
    for key in (
        "retained_regions",
        "retained_bytes",
        "allocated_batches",
        "allocated_requests",
        "requester_workers",
        "inflight_batches",
        "inflight_bytes",
    ):
        if stats[key] != 0:
            raise RuntimeError(f"{name} retains {key}: {stats[key]}")


def command(expected: str) -> None:
    if input() != expected:
        raise RuntimeError(f"expected controller command {expected}")


def serve_source(payload: bytearray, config: orbite.EngineConfig) -> None:
    with orbite.TransferEngine(config) as engine:
        options = orbite.MemoryOptions(permission=orbite.Permission.REMOTE_READ)
        with engine.register_buffer(payload, options):
            print("READY", engine.endpoint(), engine.source_epoch(), flush=True)
            command("close")


def check_pending(batch: orbite.Batch) -> None:
    tasks = batch.tasks()
    if not (
        len(tasks) == 1
        and tasks[0].state in (orbite.TaskState.WAITING, orbite.TaskState.RUNNING)
        and not tasks[0].locally_drained
        and tasks[0].transferred_bytes == 0
        and tasks[0].failure is None
    ):
        raise RuntimeError(f"READ no longer pending: {tasks}")


def read_charges(engine: orbite.TransferEngine) -> tuple[int, ...]:
    stats = engine.stats()
    return tuple(stats[key] for key in (
        "allocated_batches", "inflight_batches", "inflight_bytes",
        "retained_regions", "retained_bytes",
    ))


def check_registration_busy(reg: orbite.RegisteredMemory) -> None:
    try:
        reg.close()
    except orbite.ResourceExhaustedError:
        return
    raise RuntimeError("closed the pending READ registration")


def pending_reads(slow_peer: str, fast_peer: str) -> None:
    def submit(batch, peer, registration, offset, length):
        _, [(address, _, _)] = peer.info()
        request = orbite.TransferRequest(
            registration.address + 1, address + offset, length, orbite.Transport.TCP
        )
        batch.submit(orbite.TransferOp.READ, peer, [request])

    with (
        orbite.TransferEngine(orbite.EngineConfig()) as engine,
        engine.open_segment(*slow_peer.split()) as slow,
        engine.open_segment(*fast_peer.split()) as fast,
    ):
        dst = Destination(b"\xee" * 8)
        owner = weakref.ref(dst)
        reg = engine.register_buffer(dst)
        fast_bytes = bytearray(dst)
        with engine.allocate_batch(1) as first:
            print("OPEN", flush=True)
            command("stopped")
            submit(first, slow, reg, 0, 5)
            check_pending(first)
            del dst
            with engine.register_buffer(fast_bytes) as fast_reg, engine.allocate_batch(1) as second:
                submit(second, fast, fast_reg, 0, 3)
                finish_batch(second, "second", 1)
            observe("BYTES", "second", fast_bytes.hex())
            check_pending(first)
            if (remaining := read_charges(engine)) != (1, 1, 5, 1, 8):
                raise RuntimeError(f"unexpected charges with one READ pending: {remaining}")
            check_registration_busy(reg)
            retained = owner()
            if retained is None:
                raise RuntimeError("pending READ lost its exporter")
            print("PENDING first", flush=True)
            later_bytes = bytearray(b"\xee" * 8)
            with engine.register_buffer(later_bytes) as later_reg, engine.allocate_batch(1) as later:
                submit(later, slow, later_reg, 3, 2)
                check_pending(first)
                check_pending(later)
                if (remaining := read_charges(engine)) != (2, 2, 7, 2, 16):
                    raise RuntimeError(f"unexpected charges with two READs pending: {remaining}")
                print("PENDING first later", flush=True)
                command("resumed")
                finish_batch(later, "later", 1)
            stats = engine.stats()
            if (stats["allocated_batches"], stats["allocated_requests"]) != (1, 1):
                raise RuntimeError("expected only the first batch after closing the later one")
            observe("BYTES", "later", later_bytes.hex())
            finish_batch(first, "first", 1)
        observe("BYTES", "first", retained.hex())
        reg.close()
        del retained
        if owner() is not None or read_charges(engine) != (0, 0, 0, 0, 0):
            raise RuntimeError("drained READ retained its exporter or charges")


def bulk_config() -> orbite.EngineConfig:
    return orbite.EngineConfig(max_transfer_bytes=BULK_BYTES, handler_timeout_us=20_000_000)


def interrupted_read(peer: str, outcome: str) -> None:
    if outcome == "resume":
        state, committed, fill = orbite.TaskState.COMPLETED, BULK_BYTES, 0x6d
        failures = (None,)
    elif outcome == "cut":
        state, committed, fill = orbite.TaskState.FAILED, 0, 0xee
        # The Python receipt exposes the I/O message and the remote effect.
        failures = (
            orbite.TransferFailure("transport I/O failed: unexpected end of file", "none"),
            orbite.TransferFailure(
                f"transport I/O failed: {strerror(ECONNRESET)} (os error {ECONNRESET})", "none"
            ),
        )
    else:
        raise ValueError(f"unexpected READ outcome: {outcome}")

    dst = Destination(b"\xee" * (BULK_BYTES + 2))
    owner = weakref.ref(dst)
    options = orbite.MemoryOptions(permission=orbite.Permission.LOCAL_ONLY)
    with orbite.TransferEngine(bulk_config()) as engine:
        with (
            engine.open_segment(*peer.split()) as source,
            engine.register_buffer(dst, options) as reg,
        ):
            is_file, [(remote, size, _)] = source.info()
            if is_file or size != BULK_BYTES:
                raise RuntimeError("expected one 64 MiB memory source")

            def submit(batch: orbite.Batch, length: int) -> None:
                request = orbite.TransferRequest(
                    reg.address + 1, remote, length, orbite.Transport.TCP
                )
                batch.submit(orbite.TransferOp.READ, source, [request])

            with engine.allocate_batch(1) as warm:
                submit(warm, 1)
                finish_batch(warm, "warmup", 1)
            if dst[1] != 0x6d:
                raise RuntimeError("warm READ returned the wrong byte")
            dst[1] = 0xee
            print("OPEN", flush=True)
            command("submit")
            with engine.allocate_batch(1) as batch:
                submit(batch, BULK_BYTES)
                del dst
                check_pending(batch)
                print("PENDING read", flush=True)
                command("prefix")
                check_pending(batch)
                try:
                    batch.close()
                except orbite.BatchBusyError:
                    pass
                else:
                    raise RuntimeError("closed the pending READ batch")
                check_registration_busy(reg)
                if (charges := read_charges(engine)) != (1, 1, BULK_BYTES, 1, BULK_BYTES + 2):
                    raise RuntimeError(f"unexpected pending READ charges: {charges}")
                if owner() is None:
                    raise RuntimeError("pending READ lost its exporter")
                print("PENDING prefix", flush=True)
                command(outcome)
                [task] = batch.wait(5)
                if (
                    task.state != state
                    or not task.locally_drained
                    or task.transferred_bytes != committed
                    or task.failure not in failures
                    or batch.tasks() != [task]
                ):
                    raise RuntimeError(f"unexpected {outcome} READ receipt: {task}")
                image = owner()
                if image is None:
                    raise RuntimeError("registered destination lost its exporter")
                if (
                    len(image) != BULK_BYTES + 2
                    or (image[0], image[-1]) != (0xee, 0xee)
                    or image.count(fill, 1, -1) != BULK_BYTES
                ):
                    raise RuntimeError(f"wrong destination bytes after {outcome}")
                if task.failure is not None:
                    print(
                        f"cut: {task.failure.message}, effect={task.failure.remote_effect}",
                        file=sys.stderr,
                    )
        image.append(0)  # Unregister must release the Python buffer export.
        del image
        if owner() is not None:
            raise RuntimeError("drained READ retained its exporter")
        check_released("interrupted requester", engine)
    observe("RESULT", outcome, task.state.value, task.transferred_bytes)
    observe("BYTES", outcome, committed, "guards-intact")


def restart_read(peer: str) -> None:
    guard = b"\xee" * 5
    destination = bytearray(guard)

    def request(
        segment: orbite.RemoteSegment, registration: orbite.RegisteredMemory
    ) -> orbite.TransferRequest:
        is_file, [(address, size, _)] = segment.info()
        if is_file or size != len(guard) - 2:
            raise RuntimeError("expected one three-byte restart source")
        return orbite.TransferRequest(registration.address + 1, address, size, orbite.Transport.TCP)

    def read(segment: orbite.RemoteSegment, req: orbite.TransferRequest, label: str) -> None:
        with engine.allocate_batch(1) as batch:
            batch.submit(orbite.TransferOp.READ, segment, [req])
            finish_batch(batch, label, 1)
        observe("BYTES", label, destination.hex())

    with orbite.TransferEngine(orbite.EngineConfig()) as engine:
        with (
            engine.open_segment(*peer.split()) as old,
            engine.register_buffer(
                destination, orbite.MemoryOptions(permission=orbite.Permission.LOCAL_ONLY)
            ) as registration,
        ):
            old_request = request(old, registration)
            read(old, old_request, "old")
            destination[:] = guard
            with engine.open_segment(*input().split()) as fresh:
                if fresh.endpoint != old.endpoint or fresh.epoch == old.epoch:
                    raise RuntimeError("expected a new epoch at the original endpoint")
                try:
                    engine.open_segment(old.endpoint, old.epoch)
                except orbite.EpochMismatchError as error:
                    # Task failures carry the native message without the operation prefix.
                    epoch_message = str(error).removeprefix("open_segment: ")
                else:
                    raise RuntimeError("Describe accepted the old epoch")
                with engine.allocate_batch(1) as stale:
                    stale.submit(orbite.TransferOp.READ, old, [old_request])
                    [task] = stale.wait(5)
                    if (
                        task.state != orbite.TaskState.FAILED
                        or task.failure != orbite.TransferFailure(epoch_message, "none")
                        or not task.locally_drained
                        or task.transferred_bytes != 0
                    ):
                        raise RuntimeError(f"stale READ returned {task}")
                    if destination != guard:
                        raise RuntimeError(f"stale READ changed the destination: {destination.hex()}")
                old.close()
                with engine.allocate_batch(1) as rejected:
                    try:
                        rejected.submit(orbite.TransferOp.READ, old, [old_request])
                    except orbite.InvalidRangeError:
                        if rejected.tasks():
                            raise RuntimeError("closed handle admitted a task")
                    else:
                        raise RuntimeError("closed Python handle accepted a READ")
                if destination != guard:
                    raise RuntimeError(f"closed handle changed the destination: {destination.hex()}")
                print(
                    f"old Describe and READ: {task.failure.message}, {task.transferred_bytes} bytes, "
                    f"effect={task.failure.remote_effect}, drained, destination={destination.hex()}",
                    file=sys.stderr,
                )
                print("RETIRED old", flush=True)
                read(fresh, request(fresh, registration), "new")
        destination.append(0)  # Final unregister must release the bytearray export.
        check_released("restart requester", engine)


def transfer(
    requester: orbite.TransferEngine,
    peer: orbite.RemoteSegment | None,
    buffers: dict[str, Buffer],
    fields: list[str],
) -> None:
    if peer is None:
        raise ValueError("DESCRIBE must precede transfers")
    operation, label, slices, name, message = fields
    local, remote = buffers["local"], buffers["source"]
    requests = []
    if slices != "-":
        for span in slices.split(","):
            local_offset, remote_offset, length = map(int, span.split(":"))
            local.end(local_offset, length)
            remote.end(remote_offset, length)
            requests.append(
                orbite.TransferRequest(
                    local.address + local_offset,
                    remote.address + remote_offset,
                    length,
                    orbite.Transport.TCP,
                )
            )
    op = orbite.TransferOp.WRITE if operation == "WRITE" else orbite.TransferOp.READ
    note = None if name == "~" else orbite.Notification(text(name), text(message))
    if operation == "READ_PAIR_UNREGISTER":
        if len(requests) != 2 or note is not None or len(remote.registrations) < 2:
            raise ValueError(
                "paired read needs two requests, no notification and a registration alias"
            )
        lo, hi = sorted(requests, key=lambda req: req.local_address)
        if lo.local_address + lo.length > hi.local_address:
            raise ValueError("paired read destinations overlap")
        with requester.allocate_batch(1) as first:
            first.submit(op, peer, [requests[0]])
            with requester.allocate_batch(1) as second:
                second.submit(op, peer, [requests[1]])
                if requester.stats()["allocated_batches"] != 2:
                    raise RuntimeError("expected two live batch handles")
                remote.unregister()
                observe("LIVE", label, 2)
                finish_batch(second, f"{label}-second", 1)
            finish_batch(first, f"{label}-first", 1)
        remote.check_export()
        return
    with requester.allocate_batch(max(1, len(requests))) as batch:
        if operation == "READ_SPLIT":
            if not requests or note is not None:
                raise ValueError("split trace needs nonempty unnotified requests")
            for request in requests:
                batch.submit(op, peer, [request])
        else:
            batch.submit(op, peer, requests, note)
        finish_batch(batch, label, len(requests))


def replay(trace: Path) -> None:
    buffers: dict[str, Buffer] = {}
    peer = None
    config = orbite.EngineConfig(
        bind_addr="127.0.0.1:0", tcp=True, merge_requests=False
    )
    with (
        orbite.TransferEngine(config) as source,
        orbite.TransferEngine(config) as requester,
    ):
        engines = {"source": source, "local": requester}
        for index, line in enumerate(trace.read_text().splitlines(), 1):
            fields = line.split()
            if not fields or fields[0].startswith("#"):
                continue
            match fields:
                case ["ORBITE_CPU_TRACE", "1"]:
                    observe("VERSION", 1)
                case ["BUFFER", name, size, initial]:
                    size = int(size)
                    if (
                        name not in engines
                        or name in buffers
                        or not 0 < size <= 1 << 20
                    ):
                        raise ValueError("invalid trace buffer")
                    if initial == "pattern":
                        backing = bytearray(i % 251 for i in range(size))
                    else:
                        backing = bytearray([int(initial)]) * size
                    buffers[name] = Buffer(backing)
                case ["REGISTER", name, permission]:
                    buffer = buffers[name]
                    options = orbite.MemoryOptions(
                        permission={
                            "rw": orbite.Permission.REMOTE_READ_WRITE,
                            "r": orbite.Permission.REMOTE_READ,
                            "local": orbite.Permission.LOCAL_ONLY,
                        }[permission],
                    )
                    registration = engines[name].register_buffer(
                        buffer.backing, options
                    )
                    if buffer.registrations and registration.address != buffer.address:
                        raise RuntimeError("duplicate registration changed address")
                    buffer.registrations.append(registration)
                case ["REGISTER_BATCH", name, "r", count]:
                    count = int(count)
                    if not 0 < count <= 1024:
                        raise ValueError("invalid registration batch size")
                    buffer = buffers[name]
                    address, length = buffer.address, len(buffer.backing)
                    aliases = engines[name].register_memory_ranges(
                        [(address, length)] * count,
                        owners=[buffer.backing] * count,
                        options=orbite.MemoryOptions(
                            permission=orbite.Permission.REMOTE_READ
                        ),
                    )
                    if len(aliases) != count or any(
                        (alias.address, alias.length) != (address, length)
                        for alias in aliases
                    ):
                        raise RuntimeError("batch duplicates changed range")
                    buffer.registrations.extend(aliases)
                    observe("REGISTER_BATCH", name, len(aliases))
                case ["UNREGISTER", name]:
                    buffers[name].unregister()
                    buffers[name].check_export()
                case ["DESCRIBE", label]:
                    if peer is not None:
                        peer.close()
                    peer = requester.open_segment(
                        source.endpoint(), source.source_epoch()
                    )
                    is_file, regions = peer.info()
                    buffer = buffers["source"]
                    if is_file or any(
                        not buffer.registrations
                        or address != buffer.address
                        or length != len(buffer.backing)
                        for address, length, _ in regions
                    ):
                        raise RuntimeError(
                            "descriptor does not identify actual backing"
                        )
                    observe(
                        "DESCRIBE", label, len(regions), sum(row[1] for row in regions)
                    )
                case ["FILL", name, offset, length, value]:
                    offset, length = int(offset), int(length)
                    buffer = buffers[name]
                    buffer.backing[offset : buffer.end(offset, length)] = (
                        bytes([int(value)]) * length
                    )
                case ["BYTES", label, name, offset, length]:
                    offset, length = int(offset), int(length)
                    buffer = buffers[name]
                    observe(
                        "BYTES",
                        label,
                        buffer.backing[offset : buffer.end(offset, length)].hex(),
                    )
                case [("READ" | "WRITE" | "READ_SPLIT" | "READ_PAIR_UNREGISTER"), _, _, _, _]:
                    transfer(requester, peer, buffers, fields)
                case ["NOTIFY", label, route, name, message]:
                    note = orbite.Notification(text(name), text(message))
                    if route == "local":
                        source.send_notification(None, note)
                    elif route == "remote" and peer is not None:
                        requester.send_notification(peer, note)
                    else:
                        raise ValueError("invalid notification route")
                    observe("NOTIFY", label, "ok")
                case ["TAKE", label]:
                    messages = source.receive_notifications()
                    payload = ",".join(
                        f"{note.name.encode().hex() or '-'}:{note.message.encode().hex() or '-'}"
                        for note in messages
                    )
                    observe("TAKE", label, len(messages), payload or "-")
                case _:
                    raise ValueError(f"invalid trace instruction at line {index}")
        if any(buffer.registrations for buffer in buffers.values()):
            raise RuntimeError("trace did not unregister all references")
        if peer is not None:
            peer.close()
        for name, engine in engines.items():
            check_released(name, engine)
    observe("CLEANUP", "ok")


if __name__ == "__main__":
    installed = Path(orbite.__file__).resolve()
    if sys.prefix == sys.base_prefix or not installed.is_relative_to(
        Path(sys.prefix).resolve()
    ):
        raise RuntimeError(
            f"expected a wheel installed in the trace virtualenv, loaded {installed}"
        )
    print(f"Loaded OrbiTE from {installed}", file=sys.stderr)
    match sys.argv[1:]:
        case ["--pending-source", payload]:
            serve_source(bytearray(payload, "ascii"), orbite.EngineConfig())
        case ["--pending-source", payload, port]:
            config = orbite.EngineConfig(bind_addr=f"127.0.0.1:{int(port)}")
            serve_source(bytearray(payload, "ascii"), config)
        case ["--bulk-read-source"]:
            serve_source(bytearray(b"\x6d") * BULK_BYTES, bulk_config())
        case ["--interrupted-read", peer, outcome]:
            interrupted_read(peer, outcome)
        case ["--restart-read", peer]:
            restart_read(peer)
        case ["--pending-reads", slow, fast]:
            pending_reads(slow, fast)
        case [trace]:
            replay(Path(trace))
        case _:
            raise ValueError("expected trace path or READ role")
