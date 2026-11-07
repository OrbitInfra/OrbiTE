"""Installed Python CUDA-export controls. The native table uses a CPU shim only."""
import ctypes as C
import gc
import json
import os
from pathlib import Path
import sys
import tempfile
import time
import unittest
import weakref

import orbite as te

BOUND = 30
CPU = '--cpu' in sys.argv
if CPU:
    sys.argv.remove('--cpu')
else:
    SHIM = C.CDLL(os.environ['ORBITE_PYTHON_CUDA_SHIM'])
    SHIM.orbite_test_allocate.argtypes = [C.c_size_t, C.c_int]
    SHIM.orbite_test_allocate.restype = C.c_size_t
    SHIM.orbite_test_free.argtypes = [C.c_size_t]
    SHIM.orbite_test_hold.argtypes = [C.c_int]
    SHIM.orbite_test_fault.argtypes = [C.c_int]
    SHIM.orbite_test_count.argtypes = [C.c_int]
    SHIM.orbite_test_context.restype = C.c_size_t
    SHIM.cuCtxSetCurrent.argtypes = [C.c_void_p]


class Allocation:
    def __init__(self, n=80, device=0):
        self.address = SHIM.orbite_test_allocate(n, device)
        assert self.address
        self.nbytes = n
        self.device = device
        C.memset(self.address, 0x71, n)

    def __del__(self):
        if self.address:
            assert SHIM.orbite_test_free(self.address) == 0
            self.address = 0


class CAI:
    def __init__(self, allocation=None, version=3, stream=None):
        self.allocation = allocation or Allocation()
        self.device = self.allocation.device
        self.info = dict(version=version, shape=(self.allocation.nbytes,), typestr='|u1',
                         data=(self.allocation.address, False), strides=None)
        if version == 3:
            self.info['stream'] = stream

    @property
    def __cuda_array_interface__(self):
        return dict(self.info)

    def __dlpack_device__(self):
        return 2, self.device


class Device(C.Structure):
    _fields_ = [('kind', C.c_int32), ('id', C.c_int32)]


class Dtype(C.Structure):
    _fields_ = [('code', C.c_uint8), ('bits', C.c_uint8), ('lanes', C.c_uint16)]


class Tensor(C.Structure):
    _fields_ = [('data', C.c_void_p), ('device', Device), ('ndim', C.c_int32),
                ('dtype', Dtype), ('shape', C.POINTER(C.c_int64)),
                ('strides', C.POINTER(C.c_int64)), ('offset', C.c_uint64)]


DELETE = C.CFUNCTYPE(None, C.c_void_p)


class Legacy(C.Structure):
    _fields_ = [('tensor', Tensor), ('manager', C.c_void_p), ('deleter', DELETE)]


class Versioned(C.Structure):
    _fields_ = [('major', C.c_uint32), ('minor', C.c_uint32), ('manager', C.c_void_p),
                ('deleter', DELETE), ('flags', C.c_uint64), ('tensor', Tensor)]


CAPSULES = {}
DELETIONS = {}
NEXT_TOKEN = 0


@DELETE
def delete(pointer):
    entry = CAPSULES.pop(pointer)
    token = entry[0].token
    DELETIONS[token] = DELETIONS.get(token, 0) + 1


C.PyDLL(None).PyCapsule_New  # Verify Python's C API is present before a capsule exists.
new_capsule = C.pythonapi.PyCapsule_New
new_capsule.argtypes = [C.c_void_p, C.c_char_p, DELETE]
new_capsule.restype = C.py_object
capsule_valid = C.pythonapi.PyCapsule_IsValid
capsule_valid.argtypes = [C.c_void_p, C.c_char_p]
capsule_valid.restype = C.c_int
capsule_pointer = C.pythonapi.PyCapsule_GetPointer
capsule_pointer.argtypes = [C.c_void_p, C.c_char_p]
capsule_pointer.restype = C.c_void_p


@DELETE
def capsule_delete(capsule):
    for name in (b'dltensor', b'dltensor_versioned'):
        if capsule_valid(capsule, name):
            delete(capsule_pointer(capsule, name))
            return


class DLPack:
    def __init__(self, allocation=None, versioned=True):
        global NEXT_TOKEN
        NEXT_TOKEN += 1
        self.token = NEXT_TOKEN
        self.allocation = allocation or Allocation()
        self.versioned = versioned
        self.device = self.allocation.device
        self.tensor_device = self.device
        self.shape = [self.allocation.nbytes]
        self.strides = None
        self.offset = 0
        self.flags = 0
        self.major = 1
        self.calls = 0
        self.cached = None
        self.cache = False

    def __dlpack_device__(self):
        return 2, self.device

    def __dlpack__(self, *, stream, max_version=None):
        self.calls += 1
        assert stream == 1
        assert max_version == (1, 0) or max_version is None
        if self.cached is not None:
            return self.cached
        shape = (C.c_int64 * len(self.shape))(*self.shape)
        strides = None if self.strides is None else (C.c_int64 * len(self.strides))(*self.strides)
        tensor = Tensor(self.allocation.address, Device(2, self.tensor_device), len(shape),
                        Dtype(1, 8, 1), shape, strides, self.offset)
        managed = Versioned(self.major, 0, None, delete, self.flags, tensor) if self.versioned else Legacy(tensor, None, delete)
        pointer = C.addressof(managed)
        CAPSULES[pointer] = self, managed, shape, strides
        cap = new_capsule(pointer, b'dltensor_versioned' if self.versioned else b'dltensor', capsule_delete)
        if self.cache:
            self.cached = cap
        return cap


class LegacyProtocol(DLPack):
    def __dlpack__(self, *, stream):
        return super().__dlpack__(stream=stream)


class Base(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.engines = []
        self.registrations = {}
        self.uncertain = []
        if not CPU:
            SHIM.orbite_test_fault(0)
            SHIM.orbite_test_hold(0)
            SHIM.cuCtxSetCurrent(None)

    def tearDown(self):
        if not CPU:
            SHIM.orbite_test_hold(0)
            SHIM.orbite_test_fault(0)
        for engine in reversed(self.engines):
            if engine not in self.uncertain:
                engine.close()
        self.temp.cleanup()

    def engine(self, **extra):
        config = dict(bind_addr='127.0.0.1:0', tcp=True)
        config.update(extra)
        path = Path(self.temp.name) / f'{len(self.engines)}.json'
        path.write_text(json.dumps(config))
        e = te.TransferEngine(te.EngineConfig.from_file(path))
        self.engines.append(e)
        return e

    def until(self, predicate):
        end = time.monotonic() + BOUND
        while not predicate():
            if time.monotonic() >= end:
                self.fail('native owner boundary was not reached')
            time.sleep(.001)

    def register(self, engine, address, length, *, owner=None, options=None):
        reg = engine.register_memory(address, length, owner=owner, options=options)
        self.registrations[engine, address] = reg
        return reg

    def buffer(self, engine, value):
        reg = engine.register_buffer(value)
        self.registrations[engine, reg.address] = reg
        return reg.address

    def release(self, engine, address):
        self.registrations[engine, address].close()
        del self.registrations[engine, address]

    def unregister(self, engine, address):
        # A requester completion can precede the source handler's final loan release.
        end = time.monotonic() + BOUND
        while True:
            try:
                self.release(engine, address)
                return
            except te.ResourceExhaustedError:
                if time.monotonic() >= end:
                    raise
                time.sleep(.001)


class Cpu(Base):
    def test_cuda_owner_reports_feature_unavailable_before_export(self):
        class Claim:
            __cuda_array_interface__ = dict(version=3, shape=(8,), typestr='|u1', data=(1, False))
        class Pack:
            def __dlpack_device__(self): return 2, 0
            def __dlpack__(self, **kwargs): raise AssertionError('CPU wheel must not obtain a GPU capsule')
        e = self.engine()
        for owner in (Claim(), Pack()):
            with self.assertRaisesRegex(te.UnavailableError, 'CUDA exporters unavailable.*cuda feature'):
                self.register(e, 1, 8, owner=owner)
        with self.assertRaises(te.RegionNotFoundError):
            self.register(e, 1, 8)
        class CpuBuffer(bytearray):
            def __dlpack_device__(self): return 1, 0
            def __dlpack__(self, **kwargs): raise AssertionError('host buffer must use its memoryview')
        host = CpuBuffer(b'host')
        address = self.buffer(e, host)
        self.release(e, address)


class Cuda(Base):
    def test_cai_versions_ranges_driver_device_contiguity_and_context(self):
        e = self.engine()
        for version, stream in ((2, None), (3, None), (3, 1), (3, 2)):
            for caller in (None, 99):
                owner = CAI(version=version, stream=stream)
                address = owner.allocation.address
                SHIM.cuCtxSetCurrent(caller)
                before = SHIM.orbite_test_count(4)
                self.register(e, address + 8, 64, owner=owner)
                self.assertGreater(SHIM.orbite_test_count(4), before)
                self.assertEqual(SHIM.orbite_test_context(), caller or 0)
                self.release(e, address + 8)
                self.assertEqual(SHIM.orbite_test_context(), caller or 0)
        SHIM.cuCtxSetCurrent(None)
        for modify in (
            lambda o: o.info.update(data=(o.allocation.address, True)),
            lambda o: o.info.update(shape=(81,)),  # Python claims a larger native allocation
            lambda o: o.info.update(shape=(40,), strides=(2,)),
            lambda o: o.info.update(shape=(2**62, 8)),
            lambda o: o.info.update(stream=0),
            lambda o: setattr(o, 'device', 1),
        ):
            owner = CAI()
            modify(owner)
            with self.assertRaises(te.Error):
                self.register(e, owner.allocation.address, 16, owner=owner)
        owner = CAI()
        owner.info['shape'] = (16,)
        with self.assertRaises(te.InvalidRangeError):
            self.register(e, owner.allocation.address + 8, 16, owner=owner)
        with self.assertRaises(te.InvalidRangeError):
            self.register(e, owner.allocation.address, 8, options=te.MemoryOptions(location='cuda:1'), owner=owner)
        owner.info['data'] = (C.addressof(C.create_string_buffer(80)), False)
        with self.assertRaises(te.Error):
            self.register(e, owner.info['data'][0], 8, owner=owner)
        with self.assertRaises(te.RegionNotFoundError):
            self.register(e, owner.allocation.address, 8)

    def test_dlpack_legacy_versioned_offset_once_and_rejections(self):
        e = self.engine()
        for cls, versioned in ((DLPack, True), (DLPack, False), (LegacyProtocol, False)):
            owner = cls(versioned=versioned)
            owner.offset, owner.shape = 8, [64]
            address, token = owner.allocation.address + 8, owner.token
            self.register(e, address, 64, owner=owner)
            self.assertEqual(owner.calls, 1)
            self.assertEqual(DELETIONS.get(token, 0), 0)
            self.release(e, address)
            gc.collect()
            self.assertEqual(DELETIONS[token], 1)
        for field, value in (('flags', 1), ('flags', 2), ('flags', 4), ('tensor_device', 1),
                             ('shape', [81]), ('strides', [2]), ('major', 2), ('offset', 2**64-1)):
            owner = DLPack()
            setattr(owner, field, value)
            with self.assertRaises(te.Error):
                self.register(e, owner.allocation.address, 8, owner=owner)
            gc.collect()
            self.assertEqual(DELETIONS[owner.token], 1)
        owner = DLPack()
        owner.device = owner.tensor_device = 1
        with self.assertRaises(te.InvalidRangeError):
            self.register(e, owner.allocation.address, 8, owner=owner)
        owner = DLPack()
        owner.cache = True
        self.register(e, owner.allocation.address, 8, owner=owner)
        with self.assertRaisesRegex(te.InvalidRangeError, 'already consumed'):
            self.register(e, owner.allocation.address, 8, owner=owner)
        self.release(e, owner.allocation.address)
        self.assertEqual(DELETIONS[owner.token], 1)

    def test_bulk_aliases_keep_each_capsule(self):
        e = self.engine()
        allocation = Allocation()
        owners = [DLPack(allocation), DLPack(allocation)]
        tokens = [o.token for o in owners]
        refs = [weakref.ref(o) for o in owners]
        address = allocation.address
        regs = e.register_memory_ranges([(address, 32), (address + 32, 32)], owners=owners)
        del owners
        gc.collect()
        self.assertTrue(all(ref() is not None for ref in refs))
        e.unregister_memory_ranges(regs)
        gc.collect()
        self.assertTrue(all(ref() is None for ref in refs))
        self.assertEqual([DELETIONS[t] for t in tokens], [1, 1])

    def test_real_tcp_gpu_source_and_destination_guarded_bytes(self):
        for factory in (CAI, DLPack):
            source, client = self.engine(), self.engine()
            owner = factory()
            address = owner.allocation.address
            C.memmove(address + 8, bytes(range(64)), 64)
            self.register(source, address, 80, owner=owner)
            host = bytearray(b'x' * 80)
            local = self.buffer(client, host)
            peer = client.open_segment(source.endpoint(), source.source_epoch())
            with client.allocate_batch(1) as batch:
                batch.submit(te.TransferOp.READ, peer, [te.TransferRequest(local + 8, address + 8, 64, te.Transport.TCP)])
                self.assertEqual(batch.wait(BOUND)[0].state, te.TaskState.COMPLETED)
                self.assertEqual(host, b'x' * 8 + bytes(range(64)) + b'x' * 8)
            host[8:72] = bytes(reversed(range(64)))
            with client.allocate_batch(1) as batch:
                batch.submit(te.TransferOp.WRITE, peer, [te.TransferRequest(local + 8, address + 8, 64, te.Transport.TCP)])
                self.assertEqual(batch.wait(BOUND)[0].state, te.TaskState.COMPLETED)
            self.assertEqual(C.string_at(address, 80), b'q' * 8 + bytes(reversed(range(64))) + b'q' * 8)
            self.release(client, local)
            self.unregister(source, address)

    def transfer(self, owner):
        source, client = self.engine(), self.engine()
        host = bytearray(b'L' * 8 + bytes(range(64)) + b'R' * 8)
        remote = self.buffer(source, host)
        address = owner.allocation.address
        self.register(client, address, 80, owner=owner)
        peer = client.open_segment(source.endpoint(), source.source_epoch())
        guard = client.allocate_batch(2)
        req = te.TransferRequest(address + 8, remote + 8, 64, te.Transport.TCP)
        return source, client, host, address, guard, peer, req

    def test_pending_drop_timeout_cancel_and_alias_keep_owner_until_drain(self):
        for factory in (CAI, DLPack):
            owner = factory()
            _, client, host, address, guard, peer, req = self.transfer(owner)
            witness = weakref.ref(owner)
            SHIM.orbite_test_hold(1)
            guard.submit(te.TransferOp.READ, peer, [req])
            self.until(lambda: SHIM.orbite_test_count(0) > 0)
            with self.assertRaises(te.TimeoutError):
                guard.wait(.001)
            guard.submit(te.TransferOp.READ, peer, [req])
            guard.cancel(1)
            del owner, guard
            gc.collect()
            self.assertIsNotNone(witness())
            SHIM.orbite_test_hold(0)
            self.unregister(client, address)
            gc.collect()
            self.assertIsNone(witness())
            self.assertEqual(host, b'L' * 8 + bytes(range(64)) + b'R' * 8)

    def test_failed_staging_cleanup_keeps_exporter_and_never_retries(self):
        for factory, fault in ((CAI, 4), (DLPack, 4), (CAI, 5), (DLPack, 5)):
            owner = factory()
            _, client, _, address, guard, peer, req = self.transfer(owner)
            guard.submit(te.TransferOp.READ, peer, [req])
            self.assertEqual(guard.wait(BOUND)[0].state, te.TaskState.COMPLETED)
            guard.close()
            witness = weakref.ref(owner)
            del owner
            SHIM.orbite_test_fault(fault)  # checked event destroy or pinned-host free
            with self.assertRaises(te.UnquiescedError):
                self.release(client, address)
            self.uncertain.append(client)
            before = (SHIM.orbite_test_count(1), SHIM.orbite_test_count(6))
            SHIM.orbite_test_fault(0)
            for _ in range(2):
                with self.assertRaises(te.UnquiescedError):
                    self.release(client, address)
            gc.collect()
            self.assertIsNotNone(witness())
            self.assertEqual((SHIM.orbite_test_count(1), SHIM.orbite_test_count(6)), before)

    def test_failed_native_restore_keeps_owner_and_cleanup_is_sticky(self):
        for factory in (CAI, DLPack):
            e = self.engine()
            self.uncertain.append(e)
            owner = factory()
            witness = weakref.ref(owner)
            SHIM.orbite_test_fault(3)  # failed context-pop cleanup after native range validation
            with self.assertRaises(te.UnquiescedError):
                self.register(e, owner.allocation.address, 80, owner=owner)
            del owner
            gc.collect()
            self.assertIsNotNone(witness())
            SHIM.orbite_test_fault(0)
            for _ in range(2):
                with self.assertRaises(te.UnquiescedError):
                    e.close()
            self.assertIsNotNone(witness())


if __name__ == '__main__':
    suite = unittest.defaultTestLoader.loadTestsFromTestCase(Cpu if CPU else Cuda)
    result = unittest.TextTestRunner(verbosity=2).run(suite)
    sys.exit(not result.wasSuccessful())
