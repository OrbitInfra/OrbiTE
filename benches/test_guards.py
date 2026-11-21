"""Boundary and missing-evidence controls for the CPU guard policy."""

import json
from pathlib import Path
from tempfile import TemporaryDirectory
import unittest
import uuid

from guards import (CASES, CHECKS, CONTRACT, FILE_STATE, LIMITS, ORBITE_BASELINE, PARAMETERS,
                    POLL, RSS_METHOD, TENT_RETRY, TIMING, VERIFICATION_IO, compare_trials, evaluate)
from run import RSS_PREFIX, peak_rss


def fixture(path, revision, boot):
    path.mkdir()
    manifest = dict(status="passed", verified_rows=480, peak_rss=RSS_METHOD, orbite_revision=revision,
                    dirty=False, measurement_dirty=False, tent_revision=CONTRACT["tent_revision"],
                    boot_id=boot, sweep_id=str(uuid.uuid4()), mounts={"ext4": "/dev/a ext4 1:1 rw", "tmpfs": "none tmpfs 0:1 rw"},
                    guard_negative_controls=2, reporter_negative_controls=5, caller_threads=1, workers_per_engine=2,
                    merge=True, metrics=False, engine_max_failover_attempts=0, cpu="Model name: test", host="test",
                    kernel="test", affinity=[0, 1], rustc="test-rustc", cxx="test-cxx",
                    tent_rpc_pools={"MC_RPC_CLIENT_IO_THREADS": "2", "MC_TE_RPC_CLIENT_IO_THREADS": "2"},
                    tent_tcp_local_retry=TENT_RETRY, poll=POLL, timing=TIMING, checks=CHECKS, file_state=FILE_STATE,
                    verification_io=VERIFICATION_IO, builds={"rust": "release, io-uring", "tent": CONTRACT["cmake_options"]},
                    harness_sha256={key: "a"*64 for key in ("tent.cpp", "transfer.rs", "run.py")},
                    parameters=PARAMETERS | dict(profile=False, profile_only=False, tail_profile_only=False, ext4="/ext4", tmpfs="/dev/shm"))
    (path / "manifest.json").write_text(json.dumps(manifest))
    with (path / "results.jsonl").open("w") as out:
        for profile, impl, op, block, batch, layout in sorted(CASES):
            file = profile.startswith("io_uring-")
            for trial in range(3):
                name = f"{trial}-{impl}-{profile}-{op}-{block}-{batch}-{layout}"
                owners = 1 if profile != "cpu" and not file and layout == "contiguous" else batch
                raw = dict(verified=True, payload_bytes=block*batch, samples_ns=[1000]*100, cpu_ns=[500]*100,
                           physical_owners=owners if impl == "orbite" else None, process_threads=2,
                           direct_io=(profile == "io_uring-ext4") if file and impl == "orbite" else None)
                (path / (name + ".stdout")).write_text(json.dumps(raw))
                (path / (name + ".stderr")).write_text(f"{RSS_PREFIX}1000\n")
                print(json.dumps(dict(profile=profile, implementation=impl, op=op, block=block, batch=batch,
                                      layout=layout, trial=trial, peak_rss_kib=1000, **raw)), file=out)
    return manifest


class GuardTests(unittest.TestCase):
    def test_inclusive_thresholds_and_direction(self):
        baseline = [dict.fromkeys(LIMITS, 100)] * 3
        boundary = {metric: percent for metric, (_, percent) in LIMITS.items()}
        self.assertTrue(all(v["within_limit"] for v in compare_trials([boundary]*3, baseline).values()))
        for metric, (direction, _) in LIMITS.items():
            worse = boundary | {metric: boundary[metric] + (-1 if direction == "min" else 1)}
            result = compare_trials([worse, boundary, worse], baseline)
            self.assertEqual([key for key, v in result.items() if not v["within_limit"]], [metric])

    def test_rss_receipt(self):
        for text in ("", RSS_PREFIX+"0", RSS_PREFIX+"-2", RSS_PREFIX+"100 KiB", (RSS_PREFIX+"42\n")*2):
            with self.assertRaises(ValueError):
                peak_rss(text)

    def test_matrix_and_pair(self):
        with TemporaryDirectory() as tmp:
            anchor, after = Path(tmp)/"base", Path(tmp)/"after"
            boot = str(uuid.uuid4())
            fixture(anchor, ORBITE_BASELINE, boot)
            manifest = fixture(after, "b"*40, boot)
            self.assertEqual(evaluate(after, anchor)["counts"], {"within-limits": 160})
            partial = evaluate(after)
            self.assertEqual(partial["status"], "incomplete")
            self.assertEqual(partial["counts"], {"within-limits": 64, "baseline-missing": 96})
            for change in ({"boot_id": str(uuid.uuid4())}, {"dirty": True},
                           {"sweep_id": json.loads((anchor/"manifest.json").read_text())["sweep_id"]}):
                (after/"manifest.json").write_text(json.dumps(manifest | change))
                with self.assertRaises(ValueError):
                    evaluate(after, anchor)
            (after/"manifest.json").write_text(json.dumps(manifest))
            rows = (after/"results.jsonl").read_text().splitlines()
            row = json.loads(rows[0])
            (after/"results.jsonl").write_text("\n".join([json.dumps(row | {"peak_rss_kib": 0}), *rows[1:]]))
            with self.assertRaisesRegex(ValueError, "RSS"):
                evaluate(after)
            # Dropping the same shape in every trial must fail even if the manifest agrees.
            kept = [line for line in rows if json.loads(line)["profile"] != "shm"]
            (after/"manifest.json").write_text(json.dumps(manifest | {"verified_rows": len(kept)}))
            (after/"results.jsonl").write_text("\n".join(kept))
            with self.assertRaisesRegex(ValueError, "matrix"):
                evaluate(after)


if __name__ == "__main__":
    unittest.main()
