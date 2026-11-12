"""Release safety checks, independent of registry credentials and optimized Python."""
import importlib.util
import hashlib
import io
import json
from pathlib import Path
import shutil
import tempfile
import unittest
from unittest.mock import patch
from urllib.error import HTTPError, URLError

ROOT = Path(__file__).resolve().parents[2]


def load(name):
    spec = importlib.util.spec_from_file_location(name, ROOT / "scripts" / f"{name}.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


release = load("check-release")
contents = load("check-package-contents")
registry = load("verify-registry")
prepare = load("prepare-release")


class ReleaseControls(unittest.TestCase):
    def test_github_notes_have_tagged_links_and_no_publication_instructions(self):
        text = "[API](rust-api.md#current-public-surface) [site](https://example.com/api.md)\n## Maintainer publication procedure\nPRIVATE INSTRUCTIONS"
        result = prepare.release_notes(text, "0.1.0-beta.1")
        self.assertIn("https://github.com/OrbitInfra/OrbiTE/blob/v0.1.0-beta.1/docs/rust-api.md#current-public-surface", result)
        self.assertIn("[site](https://example.com/api.md)", result)
        self.assertNotIn("PRIVATE INSTRUCTIONS", result)

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

    def test_registry_visibility_is_polled_without_repeating_an_upload(self):
        with tempfile.TemporaryDirectory() as directory:
            files = [Path(directory) / name for name in ("first.whl", "last.whl")]
            for path in files:
                path.write_bytes(path.name.encode())
            entries = [{"filename": p.name, "digests": {"sha256": hashlib.sha256(p.read_bytes()).hexdigest()}} for p in files]
            replies = [HTTPError("https://pypi.org/fixture", 404, "not visible", {}, None),
                       io.BytesIO(json.dumps({"urls": entries[:1]}).encode()),
                       io.BytesIO(json.dumps({"urls": entries}).encode())]
            request = registry.urllib.request.Request("https://pypi.org/fixture")
            with patch.object(registry.urllib.request, "urlopen", side_effect=replies) as read, patch.object(registry.time, "sleep") as sleep:
                registry.wait_for_receipt("pypi", files, request)
            self.assertEqual(read.call_count, 3)
            self.assertEqual(sleep.call_count, 2)
            self.assertEqual(request.get_method(), "GET")
            self.assertIsNone(request.data)
            for call in read.call_args_list:
                self.assertLessEqual(call.kwargs["timeout"], 10)

    def test_registry_visibility_wait_is_bounded_and_never_accepts_missing_bytes(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "artifact"
            path.write_bytes(b"tested bytes")
            with patch.object(registry.urllib.request, "urlopen", side_effect=lambda *a, **kw: io.BytesIO(b'{"urls": []}')) as read, patch.object(registry.time, "sleep") as sleep:
                with self.assertRaises(registry.ReceiptPending):
                    registry.wait_for_receipt("pypi", [path], "https://pypi.org/fixture")
            self.assertEqual(read.call_count, 13)
            self.assertEqual(sleep.call_count, 12)
            with patch.object(registry.time, "monotonic", side_effect=[0, 61]), patch.object(registry.urllib.request, "urlopen") as read:
                with self.assertRaises(registry.ReceiptPending):
                    registry.wait_for_receipt("pypi", [path], "https://pypi.org/fixture")
            read.assert_not_called()

    def test_registry_wrong_digest_and_nonvisibility_errors_fail_immediately(self):
        with tempfile.TemporaryDirectory() as directory:
            missing = Path(directory) / "missing"
            present = Path(directory) / "present"
            missing.write_bytes(b"missing")
            present.write_bytes(b"tested bytes")
            replies = [io.BytesIO(b'{"urls": [{"filename": "present", "digests": {"sha256": "wrong"}}]}'),
                       HTTPError("https://pypi.org/fixture", 403, "denied", {}, None),
                       URLError("injected network failure")]
            for reply in replies:
                with self.subTest(reply=type(reply).__name__), patch.object(registry.urllib.request, "urlopen", side_effect=[reply]) as read, patch.object(registry.time, "sleep") as sleep:
                    with self.assertRaises((ValueError, URLError)):
                        registry.wait_for_receipt("pypi", [missing, present], "https://pypi.org/fixture")
                self.assertEqual(read.call_count, 1)
                sleep.assert_not_called()


if __name__ == "__main__":
    unittest.main()
