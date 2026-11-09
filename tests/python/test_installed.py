"""Installed-wheel contract, byte and lifetime controls (no source-tree import)."""
from dataclasses import asdict
import ctypes
import gc
import inspect
import json
import os
from pathlib import Path
import socket
import tempfile
import threading
import time
import unittest
import weakref

import orbite as te

BOUND = 30


def exact(sock, size):
    data = bytearray()
    while len(data) < size:
        piece = sock.recv(size - len(data))
        if not piece:
            raise EOFError("truncated relay frame")
        data.extend(piece)
    return data


class ReadRelay:
    """Hold a real TCP reply, after source bytes exist, before requester drain."""
    def __init__(self, endpoint):
        host, port = endpoint.rsplit(":", 1)
        self.target = host, int(port)
        self.listener = socket.socket()
        self.listener.bind(("127.0.0.1", 0))
        self.listener.listen(8)
        self.listener.settimeout(.1)
        self.endpoint = "127.0.0.1:%d" % self.listener.getsockname()[1]
        self.held = threading.Event()
        self.release = threading.Event()
        self.stop = threading.Event()
        self.errors = []
        self.workers = []
        self.thread = threading.Thread(target=self.run)
        self.thread.start()

    def run(self):
        try:
            while not self.stop.is_set():
                try:
                    client, _ = self.listener.accept()
                except TimeoutError:
                    continue
                assert len(self.workers) < 16, "fixture connection bound"
                worker = threading.Thread(target=self.serve, args=(client,))
                self.workers.append(worker)
                worker.start()
        except Exception as exc:
            self.errors.append(exc)
            self.held.set()

    def serve(self, client):
        try:
            with client, socket.create_connection(self.target, timeout=BOUND) as peer:
                client.settimeout(BOUND)
                request = exact(client, 52)
                peer.sendall(request)
                response = exact(peer, 36)
                n = int.from_bytes(response[-8:], "big")
                payload = exact(peer, n) if n else b""
                if request[10] == 1:
                    self.held.set()
                    if not self.release.wait(BOUND):
                        raise TimeoutError("release never reached")
                client.sendall(response + payload)
        except Exception as exc:
            self.errors.append(exc)
            self.held.set()

    def close(self):
        self.release.set()
        self.stop.set()
        self.thread.join(BOUND)
        self.listener.close()
        for worker in self.workers:
            worker.join(BOUND)
            assert not worker.is_alive()
        assert not self.thread.is_alive(), "relay owner did not stop"
        assert not self.errors, self.errors


class Exporter(bytearray):
    pass


class Contract(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.engines, self.relays = [], []

    def tearDown(self):
        for relay in self.relays:
            relay.close()
        for engine in reversed(self.engines):
            engine.close()
        self.temp.cleanup()

    def engine(self, **updates):
        config = dict(bind_addr="127.0.0.1:0", tcp=True)
        config.update(updates)
        path = Path(self.temp.name) / f"engine-{len(self.engines)}.json"
        path.write_text(json.dumps(config))
        engine = te.TransferEngine(te.EngineConfig.from_file(path))
        self.engines.append(engine)
        return engine

    def fixture(self, held=False):
        source, client = self.engine(), self.engine()
        src = Exporter(b"L" * 8 + bytes(range(64)) + b"R" * 8)
        dst = Exporter(b"x" * 80)
        sr, dr = source.register_buffer(src), client.register_buffer(dst)
        endpoint = source.endpoint()
        if held:
            relay = ReadRelay(endpoint)
            self.relays.append(relay)
            endpoint = relay.endpoint
        peer = client.open_segment(endpoint, source.source_epoch())
        batch = client.allocate_batch(2)
        request = te.TransferRequest(dr.address + 8, sr.address + 8, 64, te.Transport.TCP)
        return source, client, src, dst, sr, dr, peer, batch, request

    def complete(self, batch, size):
        tasks = batch.wait(BOUND)
        self.assertEqual([s.state for s in tasks], [te.TaskState.COMPLETED])
        self.assertEqual(tasks[0].transferred_bytes, size)
        self.assertTrue(tasks[0].locally_drained)
        self.assertIsNone(tasks[0].failure)
        return tasks

    def test_installed_distribution_and_surface(self):
        self.assertNotIn("/python/orbite/", str(Path(te.__file__).resolve()))
        import importlib.metadata
        self.assertEqual(importlib.metadata.version("orbite"), "0.1.0b1")
        for name in ("TransferEngine", "EngineConfig", "Memory", "RegisteredMemory", "RemoteSegment", "FileSegment", "Batch", "TransferRequest", "TaskStatus", "Notification"):
            self.assertTrue(hasattr(te, name), name)
        for name in ("TentException", "Request", "TransferStatusEnum", "TransportType", "kWildcardLocation", "LOCAL_SEGMENT_ID", "UB", "MPCOMM"):
            self.assertFalse(hasattr(te, name), name)
        self.assertEqual({name: value.value for name, value in te.Transport.__members__.items()},
                         {"UNSPEC": 0, "TCP": 1, "HP_TCP": 2, "SHM": 3, "RDMA": 4,
                          "NVLINK": 6, "MNNVL": 7, "IO_URING": 8, "GDS": 9})
        with self.assertRaises(ValueError):
            te.Transport(5)
        self.assertEqual(asdict(te.TransferRequest(10, 20, 4)), dict(local_address=10,remote_address=20,length=4,transport=te.Transport.UNSPEC,priority=te.Priority.HIGH,policy_name=None,deadline_ns=0,intent=te.IntentType.UNSPEC))
        self.assertEqual(te.MemoryOptions().permission, te.Permission.REMOTE_READ_WRITE)
        with self.assertRaises(AttributeError):
            te.TransferRequest(1, 2, 3).typo = 1
        for name in ("ConfigError", "InvalidRangeError", "RegionNotFoundError", "UnavailableError", "ResourceExhaustedError", "TimeoutError", "UnquiescedError", "EpochMismatchError", "InternalError"):
            self.assertTrue(issubclass(getattr(te, name), te.Error))

    def test_policy_intent_deadline_and_permissions_reach_owner(self):
        engine = self.engine(selector={"policy": [
            {"name": "background", "segment_type": "memory", "intent_type": "background_prefetch", "priority": "low", "transports": ["tcp"]},
            {"name": "no_route", "segment_type": "memory", "transports": ["nvlink"]},
        ]})
        data = bytearray(b"!!data" + b"!" * 26)
        reg = engine.register_buffer(data)
        peer = engine.open_segment(engine.endpoint(), engine.source_epoch())
        batch = engine.allocate_batch(4)
        request = te.TransferRequest(reg.address + 16, reg.address + 2, 4, priority=te.Priority.LOW)
        for policy, intent, priority in (("unknown", te.IntentType.UNSPEC, te.Priority.LOW), (None, te.IntentType.UNSPEC, te.Priority.LOW), ("no_route", te.IntentType.BACKGROUND_PREFETCH, te.Priority.LOW), (None, te.IntentType.BACKGROUND_PREFETCH, te.Priority.HIGH)):
            request.policy_name, request.intent, request.priority = policy, intent, priority
            with self.assertRaises(te.Error):
                batch.submit(te.TransferOp.READ, peer, [request])
        self.assertEqual(batch.tasks(), [])
        request.policy_name, request.intent, request.priority, request.deadline_ns = "background", te.IntentType.BACKGROUND_PREFETCH, te.Priority.LOW, 1
        try:
            batch.submit(te.TransferOp.READ, peer, [request])
        except te.Error:
            pass
        else:
            self.assertIn(batch.wait(BOUND)[0].state, (te.TaskState.FAILED, te.TaskState.CANCELLED))
        self.assertEqual(data[16:], b"!" * 16)
        batch.close()
        batch = engine.allocate_batch(1)
        request.deadline_ns = time.monotonic_ns() + 10_000_000_000
        batch.submit(te.TransferOp.READ, peer, [request])
        self.complete(batch, 4)
        self.assertEqual(data, b"!!data" + b"!" * 10 + b"data" + b"!" * 12)
        batch.close()
        reg.close()
        reg = engine.register_memory(reg.address, len(data), owner=data, options=te.MemoryOptions(permission=te.Permission.REMOTE_READ))
        # Refresh the peer's descriptor after changing source permissions.
        peer.close()
        peer = engine.open_segment(engine.endpoint(), engine.source_epoch())
        with engine.allocate_batch(1) as batch:
            with self.assertRaises(te.PermissionDeniedError):
                batch.submit(te.TransferOp.WRITE, peer, [request])
        self.assertEqual(data[2:6], b"data")

    def test_two_process_tcp_installed_wheel(self):
        import subprocess, sys, select
        code = r'''
import json, sys, orbite as te
engine = te.TransferEngine(te.EngineConfig.from_file(sys.argv[1]))
memory = bytearray(b"LLsourceRR")
registration = engine.register_buffer(memory)
print(json.dumps([engine.endpoint(), engine.source_epoch(), registration.address]), flush=True)
assert sys.stdin.readline().strip() == "done"
assert memory == b"LLtargetRR", memory
registration.close()
engine.close()
'''
        client = self.engine()
        cfg = str(Path(self.temp.name) / "engine-0.json")
        with subprocess.Popen([sys.executable, "-I", "-c", code, cfg], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True) as child:
            try:
                self.assertTrue(select.select([child.stdout], [], [], BOUND)[0])
                endpoint, epoch, remote = json.loads(child.stdout.readline())
                data = bytearray(b"xx------yy")
                reg = client.register_buffer(data)
                peer = client.open_segment(endpoint, epoch)
                request = te.TransferRequest(reg.address + 2, remote + 2, 6, te.Transport.TCP)
                with client.allocate_batch(1) as batch:
                    batch.submit(te.TransferOp.READ, peer, [request])
                    self.complete(batch, 6)
                self.assertEqual(data, b"xxsourceyy")
                data[2:8] = b"target"
                with client.allocate_batch(1) as batch:
                    batch.submit(te.TransferOp.WRITE, peer, [request])
                    self.complete(batch, 6)
                out, err = child.communicate("done\n", timeout=BOUND)
                self.assertEqual(child.returncode, 0, (out, err))
                self.assertEqual(data, b"xxtargetyy")
            finally:
                if child.poll() is None:
                    child.kill()
                    child.wait()

    def test_real_tcp_read_write_guards_status_and_notifications(self):
        source, client, src, dst, sr, dr, peer, batch, req = self.fixture()
        self.assertIsInstance(client.local_topology(), dict)
        self.assertIn(sr.address, [row[0] for row in peer.info()[1]])
        batch.submit(te.TransferOp.READ, peer, [req], te.Notification("ready", "read"))
        tasks = self.complete(batch, 64)
        self.assertEqual(dst, b"x" * 8 + bytes(range(64)) + b"x" * 8)
        self.assertEqual(src, b"L" * 8 + bytes(range(64)) + b"R" * 8)
        self.assertEqual(batch.tasks(), tasks)
        self.assertEqual(batch.progress(), tasks)
        self.assertEqual(tasks[0].physical_owner, 0)
        self.assertEqual(source.receive_notifications(), [te.Notification("ready", "read")])
        batch.close()
        dst[8:72] = bytes(reversed(range(64)))
        with client.allocate_batch(1) as batch:
            batch.submit(te.TransferOp.WRITE, peer, [req], te.Notification("ready", "write"))
            self.complete(batch, 64)
        self.assertEqual(src, b"L" * 8 + bytes(reversed(range(64))) + b"R" * 8)
        self.assertEqual(source.receive_notifications(), [te.Notification("ready", "write")])
        client.send_notification(peer, te.Notification("direct", "\0unicode λ"))
        self.assertEqual(source.receive_notifications(), [te.Notification("direct", "\0unicode λ")])
        client.send_notification(None, te.Notification("self", "ok"))
        self.assertEqual(client.receive_notifications(), [te.Notification("self", "ok")])
        peer.close()
        with self.assertRaises(te.InvalidRangeError):
            peer.info()
        with self.assertRaises(te.EpochMismatchError):
            client.open_segment(source.endpoint(), client.source_epoch())
        peer = client.open_segment(source.endpoint(), source.source_epoch())
        self.assertEqual(peer.epoch, source.source_epoch())
        dr.close()
        sr.close()
        self.assertEqual(client.stats()["retained_bytes"], 0)

    def test_registration_options_lists_exceptions_and_limits(self):
        engine = self.engine()
        a, b = engine.allocate_memory(64), engine.allocate_memory(64, te.MemoryOptions())
        ranges = [(a.address, 64), (b.address, 64)]
        for permission in (te.Permission.REMOTE_READ, te.Permission.REMOTE_READ_WRITE):
            regs = engine.register_memory_ranges(ranges, options=te.MemoryOptions(permission=permission))
            self.assertEqual(len(regs), 2)
            for reg in regs:
                reg.close()
        self.assertEqual(engine.register_memory_ranges([]), [])
        a.close(); b.close()
        with self.assertRaises(te.InvalidRangeError):
            engine.allocate_memory(0)
        with self.assertRaises(te.RegionNotFoundError):
            engine.register_memory(1, 8)
        with self.assertRaises(te.InvalidRangeError):
            engine.register_memory_ranges([(1, 8)], owners=[])
        with self.assertRaises(te.InvalidRangeError):
            engine.allocate_memory(8, te.MemoryOptions(transport=5))
        with self.assertRaises((te.InvalidRangeError, BufferError, TypeError)):
            engine.register_buffer(b"immutable")
        view = bytearray(32)
        addr = ctypes.addressof(ctypes.c_char.from_buffer(view))
        reg = engine.register_memory(addr + 4, 8, owner=view)
        with self.assertRaises(BufferError):
            view.extend(b"resize")
        with self.assertRaises(te.InvalidRangeError):
            engine.register_memory(addr + 30, 8, owner=view)
        reg.close()
        view.extend(b"resize")

    def test_context_manager_exits_and_engine_keepalive(self):
        engine = self.engine()
        for allocate in (lambda: engine.allocate_memory(4096), lambda: engine.allocate_memory(4096, te.MemoryOptions()), lambda: engine.allocate_batch(1)):
            with allocate() as value:
                self.assertIsNotNone(value)
            expected = ValueError("with body")
            with self.assertRaises(ValueError) as raised:
                with allocate():
                    raise expected
            self.assertIs(raised.exception, expected)
        del allocate, value
        memory = engine.allocate_memory(16)
        self.assertEqual(memory.length, 16)
        self.assertGreater(memory.address, 0)
        batch = engine.allocate_batch(1)
        batch.close(); batch.close()
        with self.assertRaises(te.InvalidRangeError):
            batch.tasks()
        ref = weakref.ref(engine)
        self.engines.remove(engine)
        del engine
        gc.collect()
        self.assertIsNotNone(ref())
        del memory, batch
        gc.collect()
        self.assertIsNone(ref())

    def test_config_json_file_environment_snapshot_and_unknown_fields(self):
        for cfg in ({"metadata_type": "etcd"}, {"metadata_servers": "https://secret-token.example"}, {"tcp": "true"}, {"typo": 1}):
            with self.assertRaises(te.ConfigError) as error:
                self.engine(**cfg)
            self.assertNotIn("secret-token", str(error.exception))
        import subprocess, sys
        code = '''
import os, orbite
config = orbite.EngineConfig.from_env()
os.environ['ORBITE_CONF'] = '{"tcp":"invalid"}'
with orbite.TransferEngine(config) as engine:
    assert engine.endpoint()
with orbite.TransferEngine(orbite.EngineConfig.from_json('{}')): pass
try: orbite.TransferEngine()
except orbite.ConfigError: pass
else: raise AssertionError('fresh environment must be validated')
'''
        env = dict(os.environ, ORBITE_CONF='{"bind_addr":"127.0.0.1:0"}', MC_TENT_CONF='{"metadata_type":"redis"}')
        subprocess.run([sys.executable, "-I", "-c", code], env=env, check=True, timeout=BOUND)

    def test_gil_release_during_blocking_wait(self):
        _, _, _, dst, sr, dr, peer, batch, req = self.fixture(held=True)
        batch.submit(te.TransferOp.READ, peer, [req])
        relay = self.relays[-1]
        self.assertTrue(relay.held.wait(BOUND))
        entered, finished, outcomes = threading.Event(), threading.Event(), []
        def wait():
            entered.set()
            try:
                outcomes.extend(batch.wait(5))
            except Exception as error:
                outcomes.append(error)
            finally:
                finished.set()
        thread = threading.Thread(target=wait)
        thread.start()
        self.assertTrue(entered.wait(BOUND))
        time.sleep(.05)
        self.assertEqual(sum(range(10000)), 49995000)
        self.assertFalse(finished.is_set(), outcomes)
        self.assertEqual(batch.tasks()[0].state, te.TaskState.RUNNING)
        relay.release.set()
        thread.join(BOUND)
        self.assertFalse(thread.is_alive())
        self.assertEqual([s.state for s in outcomes], [te.TaskState.COMPLETED])
        self.assertEqual(outcomes[0].transferred_bytes, 64)
        self.assertEqual(dst, b"x" * 8 + bytes(range(64)) + b"x" * 8)
        batch.close()

    def test_timeout_cancel_and_early_drop_retain_exporter_until_drain(self):
        _, _, _, dst, sr, dr, peer, batch, req = self.fixture(held=True)
        ref = weakref.ref(dst)
        batch.submit(te.TransferOp.READ, peer, [req])
        relay = self.relays[-1]
        self.assertTrue(relay.held.wait(BOUND))
        with self.assertRaises(te.TimeoutError):
            batch.wait(.001)
        with self.assertRaises(te.UnavailableError):
            batch.cancel(0)
        batch.submit(te.TransferOp.READ, peer, [te.TransferRequest(dr.address, sr.address, 8, te.Transport.TCP)])
        batch.cancel(1)
        with self.assertRaises(te.ResourceExhaustedError):
            dr.close()
        del dst, batch
        gc.collect()
        self.assertIsNotNone(ref())
        relay.release.set()
        until = time.monotonic() + BOUND
        while True:
            try:
                dr.close()
                break
            except te.ResourceExhaustedError:
                if time.monotonic() > until:
                    raise
                time.sleep(.001)
        gc.collect()
        self.assertIsNone(ref())

    def test_queued_cancel_status_and_destination_guards(self):
        _, _, _, dst, sr, dr, peer, batch, req = self.fixture(held=True)
        batch.submit(te.TransferOp.READ, peer, [req])
        relay = self.relays[-1]
        self.assertTrue(relay.held.wait(BOUND))
        batch.submit(te.TransferOp.READ, peer, [te.TransferRequest(dr.address, sr.address, 8, te.Transport.TCP)])
        batch.cancel(1)
        relay.release.set()
        self.assertEqual([s.state for s in batch.wait(BOUND)], [te.TaskState.COMPLETED, te.TaskState.CANCELLED])
        self.assertEqual(dst, b"x" * 8 + bytes(range(64)) + b"x" * 8)
        batch.close()

    def test_memory_guard_early_drop_preserves_pending_destination(self):
        _, client, _, _, sr, dr, peer, initial, _ = self.fixture(held=True)
        memory = client.allocate_memory(80)
        addr = memory.address
        ctypes.memset(addr, 0x71, 80)
        reg = client.register_memory(addr, 80)
        batch = client.allocate_batch(1)
        batch.submit(te.TransferOp.READ, peer, [te.TransferRequest(addr + 8, sr.address + 8, 64, te.Transport.TCP)])
        relay = self.relays[-1]
        self.assertTrue(relay.held.wait(BOUND))
        del memory
        gc.collect()
        self.assertEqual(ctypes.string_at(addr, 80), b"q" * 80)
        relay.release.set()
        self.complete(batch, 64)
        self.assertEqual(ctypes.string_at(addr, 80), b"q" * 8 + bytes(range(64)) + b"q" * 8)
        batch.close()
        reg.close()
        self.assertEqual(client.stats()["retained_bytes"], dr.length)

    def test_file_segments_and_unavailable_feature(self):
        engine = self.engine()
        path = Path(self.temp.name) / 'data'
        path.write_bytes(b'LlocalRR')
        with engine.open_file_segment(path, te.Permission.REMOTE_READ) as file:
            self.assertEqual(file.info()[0], True)
            self.assertEqual(file.info()[1][0][1], 8)
            memory = bytearray(b'????????')
            reg = engine.register_buffer(memory)
            with engine.allocate_batch(1) as batch:
                with self.assertRaises(te.UnavailableError):
                    batch.submit(te.TransferOp.READ, file, [te.TransferRequest(reg.address, 0, 8, te.Transport.TCP)])
            self.assertEqual(memory, b'????????')


if __name__ == '__main__':
    unittest.main()
