"""Artifact rejection must survive -O and PYTHONOPTIMIZE, without publishing."""
import io
import os
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile
import unittest
import zipfile

ROOT = Path(__file__).resolve().parents[2]
CHECK = ROOT / "scripts/check-python-artifacts.py"
VERSION = "0.1.0b1"


class ArtifactValidation(unittest.TestCase):
    def test_grafted_native_libraries_are_rejected_with_optimized_python(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / f"orbite-{VERSION}-cp313-cp313-manylinux_2_28_x86_64.whl"
            for library in ("libibverbs.so.1", "libibverbs-deadbeef.so.1", "libmlx5.so", "librdmacm.so.1", "libcuda.so.1", "libcudart.so", "libcufile.so.0"):
                with self.subTest(library=library):
                    with zipfile.ZipFile(path, "w") as archive:
                        archive.writestr(f"orbite-{VERSION}.dist-info/METADATA", f"Name: orbite\nVersion: {VERSION}\n")
                        archive.writestr(f"orbite.libs/{library}", b"fixture")
                    result = subprocess.run([sys.executable, "-O", str(CHECK), str(path)], capture_output=True, text=True)
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn("vendored native runtime", result.stderr)

    def test_metadata_checks_survive_optimization(self):
        with tempfile.TemporaryDirectory(prefix="orbite-artifact-check-") as directory:
            root = Path(directory)
            for kind in ("wheel", "sdist"):
                for fault in (None, "name", "version", "filename", "missing", "ambiguous"):
                    if kind == "sdist" and fault == "ambiguous":
                        continue
                    metadata = (f"Name: {'wrong' if fault == 'name' else 'orbite'}\n"
                                f"Version: {'0.0.0' if fault == 'version' else VERSION}\n").encode()
                    name = "wrong" if fault == "filename" else "orbite"
                    if kind == "wheel":
                        artifact = root / f"{name}-{VERSION}-py3-none-any.whl"
                        with zipfile.ZipFile(artifact, "w") as archive:
                            if fault != "missing":
                                archive.writestr(f"orbite-{VERSION}.dist-info/METADATA", metadata)
                            if fault == "ambiguous":
                                archive.writestr("second.dist-info/METADATA", metadata)
                    else:
                        artifact = root / f"{name}-{VERSION}.tar.gz"
                        with tarfile.open(artifact, "w:gz") as archive:
                            if fault != "missing":
                                info = tarfile.TarInfo(f"orbite-{VERSION}/PKG-INFO")
                                info.size = len(metadata)
                                archive.addfile(info, io.BytesIO(metadata))
                    for flags, optimize in (([], "0"), (["-O"], "0"), ([], "2")):
                        with self.subTest(kind=kind, fault=fault, flags=flags, optimize=optimize):
                            result = subprocess.run(
                                [sys.executable, *flags, str(CHECK), str(artifact)],
                                env={**os.environ, "PYTHONOPTIMIZE": optimize},
                                capture_output=True, text=True,
                            )
                            if fault is None:
                                self.assertEqual(result.returncode, 0, result.stderr)
                            else:
                                self.assertNotEqual(result.returncode, 0, result.stdout)


if __name__ == "__main__":
    unittest.main()
