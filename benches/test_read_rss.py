"""Controls for the workloads and file ownership of the READ memory diagnostic."""
from contextlib import redirect_stderr
from io import StringIO
import json
from pathlib import Path
from tempfile import TemporaryDirectory
import unittest
from unittest.mock import patch

from profile_read_rss import measure
from run import parse_args


class ReadRssTests(unittest.TestCase):
    def test_one_fixed_shape_per_dispatch(self):
        args = ["--output", "/tmp/rss-results", "--reference", "/tmp/native", "--ext4", "/tmp/ext4"]
        modes = ["--tcp-read-rss-only", "--tmpfs-read-rss-only"]
        with patch("platform.system", return_value="Linux"):
            for mode in modes:
                p = parse_args([*args, mode])
                self.assertEqual((p.sizes, p.batches, p.warmup, p.samples, p.workers, p.trials),
                                 ([64 << 20], [1], 3, 100, 2, 3))
            for invalid in (modes, [modes[1], "--profile"], [modes[1], "--tmpfs-tail-baseline", "/tmp/frozen"],
                            [modes[1], "--samples", "99"]):
                with self.subTest(invalid=invalid), redirect_stderr(StringIO()), self.assertRaises(SystemExit):
                    parse_args([*args, *invalid])

    def test_wrong_route_rejects_the_row_and_removes_the_private_file(self):
        with TemporaryDirectory() as tmp:
            root = Path(tmp)
            mount = root / "mount"
            mount.mkdir()

            def wrong_route(label, command, overrides):
                self.assertEqual(label, "orbite-rss-0")
                self.assertEqual(command[-9:-1], ["io_uring", "read", 64 << 20, 1, "contiguous", 3, 100, 2])
                path = command[-1]
                self.assertEqual(path.parent.parent, mount)
                path.write_bytes(b"fixture")
                return json.dumps(dict(verified=True, payload_bytes=64 << 20, physical_owners=1,
                                       direct_io=True, samples_ns=[1]*100, cpu_ns=[1]*100))

            with self.assertRaisesRegex(ValueError, "expected buffered io_uring"):
                measure(root, "rust", "tent", wrong_route, {}, tmpfs=mount)
            self.assertEqual(list(mount.iterdir()), [])
            self.assertFalse((root / "tmpfs-read-rss.json").exists())
