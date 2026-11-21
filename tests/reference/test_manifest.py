"""Reject incomplete copies of a real CPU oracle bundle without changing the original."""

import argparse
from copy import deepcopy
from pathlib import Path
from shutil import copyfile
import sys
from tempfile import TemporaryDirectory

sys.path.insert(0, str(Path(__file__).resolve().parent))
from manifest import PROFILES, load_manifest, validate


def controls(path):
    original = load_manifest(path)
    status = validate(original, path.parent)
    with TemporaryDirectory() as tmp:
        output = Path(tmp)
        for log in path.parent.iterdir():
            if log.suffix in (".stdout", ".stderr"):
                copyfile(log, output / log.name)
        validate(original, output, status)

        def rejected(label, altered, expected=status):
            try:
                validate(altered, output, expected)
            except (ValueError, OSError):
                return
            raise AssertionError(f"accepted {label} for {status}")

        def bad_log(name, replacement):
            log = output / name
            if replacement is None:
                log.unlink()
            else:
                log.write_text(replacement)
            try:
                rejected(name, original)
            finally:
                copyfile(path.parent / name, log)

        for other in PROFILES.keys() - {status}:
            rejected("other requested profile", original, other)
        for state in ("running", "failed", "unknown"):
            rejected(state, original | {"status": state}, None)
        rejected("error in passing manifest", original | {"error": "cleanup failed"})
        rejected(
            "missing comparator control", original | {"negative_controls": original["negative_controls"][:-1]}
        )
        for driver in original["pending_read"]:
            trace = f"{driver}-trace.stdout"
            bad_log(trace, None)
            bad_log(trace, (output / trace).read_text().replace("OBS\tCLEANUP\tok", "OBS\tCLEANUP\tbroken"))
            for case in ("pending_read", "interrupted_read", "restart_read"):
                if case not in original:
                    continue
                changed = deepcopy(original)
                del changed[case][driver]
                rejected(f"missing {case}.{driver}", changed)
                changed = deepcopy(original)
                changed[case][driver][-1] = changed[case][driver][-2]
                rejected(f"replaced cleanup of {case}.{driver}", changed)
                name = driver + "-" + case.replace("_", "-")
                changed = deepcopy(original)
                [record] = [c for c in changed["commands"] if c["name"] == name]
                record["timed_out"] = True
                rejected(f"timed out {name}", changed)
                bad_log(name + ".stdout", "\n".join(original[case][driver][::-1]) + "\n")
            if "interrupted_read" in original:
                bad_log(driver + "-interrupted-read.stderr", "")
        if "unavailable_controls" in original:
            rejected("portable without a reason", original | {"unavailable_controls": {}})
        if "tent" in original["pending_read"]:
            bad_log("tebench-target.stdout", "READY 127.0.0.1:0\nFLIPPED 8191\nRESTORED\nCLOSED\n")
            bad_log("tebench-corrupt.stderr", "")
            bad_log("tebench-restored.stdout", "")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("manifest", type=Path)
    args = parser.parse_args()
    controls(args.manifest)
    print(f"CPU manifest relocation and rejection controls passed: {args.manifest}")
