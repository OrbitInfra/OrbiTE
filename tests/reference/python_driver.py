"""Replay the shared valid-input CPU trace through an installed OrbiTE wheel."""

from __future__ import annotations

import sys
import weakref
from dataclasses import dataclass, field
from pathlib import Path

import orbite


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


def command(expected: str) -> None:
    if input() != expected:
        raise RuntimeError(f"expected controller command {expected}")


def pending_source(payload: str) -> None:
    with orbite.TransferEngine(orbite.EngineConfig()) as engine:
        options = orbite.MemoryOptions(permission=orbite.Permission.REMOTE_READ)
        with engine.register_buffer(bytearray(payload, "ascii"), options):
            print("READY", engine.endpoint(), engine.source_epoch(), flush=True)
            command("close")


def pending_reads(slow_peer: str, fast_peer: str) -> None:
    class Destination(bytearray):
        """A weak reference can observe the backing without keeping it alive."""

    def check_pending(batch):
        tasks = batch.tasks()
        if not (
            len(tasks) == 1
            and tasks[0].state in (orbite.TaskState.WAITING, orbite.TaskState.RUNNING)
            and not tasks[0].locally_drained
            and tasks[0].transferred_bytes == 0
        ):
            raise RuntimeError(f"READ no longer pending: {tasks}")

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
        def charges():
            stats = engine.stats()
            return tuple(stats[key] for key in (
                "allocated_batches", "inflight_batches", "inflight_bytes",
                "retained_regions", "retained_bytes",
            ))

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
            if (remaining := charges()) != (1, 1, 5, 1, 8):
                raise RuntimeError(f"unexpected charges with one READ pending: {remaining}")
            try:
                reg.close()
            except orbite.ResourceExhaustedError:
                pass  # The Python registration cannot close while the batch owns its exporter.
            else:
                raise RuntimeError("closed the pending Python registration")
            retained = owner()
            if retained is None:
                raise RuntimeError("pending READ lost its exporter")
            print("PENDING first", flush=True)
            later_bytes = bytearray(b"\xee" * 8)
            with engine.register_buffer(later_bytes) as later_reg, engine.allocate_batch(1) as later:
                submit(later, slow, later_reg, 3, 2)
                check_pending(first)
                check_pending(later)
                if (remaining := charges()) != (2, 2, 7, 2, 16):
                    raise RuntimeError(f"unexpected charges with two READs pending: {remaining}")
                print("PENDING first later", flush=True)
                command("resumed")
                finish_batch(later, "later", 1)
            observe("BYTES", "later", later_bytes.hex())
            finish_batch(first, "first", 1)
        observe("BYTES", "first", retained.hex())
        reg.close()
        del retained
        if owner() is not None or charges() != (0, 0, 0, 0, 0):
            raise RuntimeError("drained READ retained its exporter or charges")


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
            pending_source(payload)
        case ["--pending-reads", slow, fast]:
            pending_reads(slow, fast)
        case [trace]:
            replay(Path(trace))
        case _:
            raise ValueError("expected trace path or pending READ role")
