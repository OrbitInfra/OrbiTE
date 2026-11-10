"""Installed combined wheel on a Linux host with no verbs/CUDA/cuFile DSOs."""
import ctypes
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

import orbite


class NativeIsolation(unittest.TestCase):
    def test_libraries_are_absent_and_default_engine_never_loads_them(self):
        self.assertIn("site-packages", orbite.__file__)
        for name in ("libibverbs.so.1", "libcuda.so.1", "libcufile.so.0"):
            with self.assertRaises(OSError, msg=name):
                ctypes.CDLL(name)
        with orbite.TransferEngine(orbite.EngineConfig()):
            maps = Path("/proc/self/maps").read_text()
            for name in ("libibverbs", "libcuda", "libcufile"):
                self.assertNotIn(name, maps)

    def test_configured_providers_name_the_missing_library(self):
        for config, library in (({"rdma": {}}, "libibverbs.so.1"),
                                ({"nvlink": True}, "libcuda.so.1")):
            with self.assertRaises(orbite.UnavailableError) as error:
                orbite.TransferEngine(orbite.EngineConfig(**config))
            self.assertIn(library, str(error.exception))
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "file"
            path.write_bytes(b"source")
            with orbite.TransferEngine(orbite.EngineConfig()) as engine:
                data = bytearray(b"guard!")
                with engine.register_buffer(data) as memory, engine.open_file_segment(path) as file:
                    with engine.allocate_batch(1) as batch:
                        with self.assertRaises(orbite.UnavailableError) as error:
                            batch.submit(orbite.TransferOp.READ, file, [orbite.TransferRequest(
                                memory.address, 0, 6, orbite.Transport.GDS)])
                        self.assertIn("libcufile.so.0", str(error.exception))
                    self.assertEqual(data, b"guard!")
                    self.assertEqual(path.read_bytes(), b"source")

    def test_gpu_free_quickstart_from_installed_full_wheel(self):
        subprocess.run([sys.executable, "-I", "-m", "orbite.quickstart"], check=True, timeout=90)


if __name__ == "__main__":
    unittest.main(verbosity=2)
