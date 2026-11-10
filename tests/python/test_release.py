"""Release safety checks, independent of registry credentials and optimized Python."""
import importlib.util
import hashlib
from pathlib import Path
import shutil
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


def load(name):
    spec = importlib.util.spec_from_file_location(name, ROOT / "scripts" / f"{name}.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


release = load("check-release")
contents = load("check-package-contents")
registry = load("verify-registry")


class ReleaseControls(unittest.TestCase):
    def test_versions_and_tag_gate(self):
        rust, python = release.versions()
        self.assertEqual((rust, python), ("0.1.0-beta.1", "0.1.0b1"))
        for event in ("pull_request", "workflow_dispatch"):
            self.assertFalse(release.selection(rust, "refs/heads/main", event, True)[1])
        self.assertFalse(release.selection(rust, "refs/tags/v" + rust, "pull_request", False)[1])
        self.assertTrue(release.selection(rust, "refs/tags/v" + rust, "push", True)[1])
        for ref, event, dry in (("refs/tags/v0.0.1", "push", True), ("refs/tags/v0.0.1", "workflow_dispatch", True), ("refs/heads/main", "workflow_dispatch", False)):
            with self.subTest(ref=ref, event=event), self.assertRaises(ValueError):
                release.selection(rust, ref, event, dry)

    def test_every_version_source_is_checked(self):
        paths = ("Cargo.toml", "Cargo.lock", "crates/orbite-python/Cargo.toml", "pyproject.toml", "README.md", "docs/releases.md", "LICENSE")
        for fault in paths[:-1]:
            with tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                for name in paths:
                    dest = root / name
                    dest.parent.mkdir(parents=True, exist_ok=True)
                    shutil.copyfile(ROOT / name, dest)
                path = root / fault
                text = path.read_text()
                if fault == "pyproject.toml":
                    text = text.replace('dynamic = ["version"]', 'version = "0.0.1"\ndynamic = []')
                else:
                    text = text.replace("0.1.0-beta.1", "0.0.1")
                path.write_text(text)
                with self.subTest(fault=fault), self.assertRaises(ValueError):
                    release.versions(root)

    def test_package_outputs_are_rejected_even_if_tracked(self):
        for name in ("logs/job.log", "docs/html/index.html", "run-output/host.json", "target/core.so", "tests/raw.trace", "../outside", "/absolute", "surprise.txt", "tests/hardware/result.json"):
            tracked = {"LICENSE", name} if name != "surprise.txt" else {"LICENSE"}
            with self.subTest(name=name), self.assertRaises(ValueError):
                contents.check_names(["LICENSE", name], tracked)
        contents.check_names(["LICENSE", "tests/fixtures/cpu-v1.trace"], {"LICENSE", "tests/fixtures/cpu-v1.trace"})
        with self.assertRaises(ValueError):
            contents.check_names(["LICENSE", "LICENSE"], {"LICENSE"})
        with self.assertRaises(ValueError):
            contents.check_names(["orbite-0.1.0b1.dist-info/licenses/LICENSE", "orbite/extra.py"], set(), wheel=True)

    def test_registry_success_requires_the_exact_bytes(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "artifact"
            path.write_bytes(b"tested bytes")
            digest = hashlib.sha256(path.read_bytes()).hexdigest()
            for name, good, bad in (
                ("pypi", {"urls": [{"filename": "artifact", "digests": {"sha256": digest}}]}, {"urls": []}),
                ("crates-io", {"version": {"checksum": digest}}, {"version": {"checksum": "wrong"}}),
            ):
                registry.verify(name, [path], good)
                with self.subTest(registry=name), self.assertRaises(ValueError):
                    registry.verify(name, [path], bad)
            path.write_bytes(b"untested replacement")
            with self.assertRaises(ValueError):
                registry.verify("crates-io", [path], good)


if __name__ == "__main__":
    unittest.main()
