"""Score the formal untraced CPU workload matrix against its frozen references."""

import argparse
import collections
from fractions import Fraction
import json
from pathlib import Path
import re
import statistics
import sys
import uuid

from compare import load, match_runs
from run import (BATCHES, CHECKS, CONTRACT, FILE_STATE, POLL, PROFILES, RSS_METHOD, SIZES,
                 TENT_RETRY, TIMING, VERIFICATION_IO, peak_rss, workload_shapes)


ORBITE_BASELINE = "12d1e83964a596d9b578e9d2879b5cd78e776da6"
LIMITS = {
    "goodput_GiB_s": ("min", 95),
    "p99_ns": ("max", 105),
    "cpu_ns_per_byte": ("max", 110),
    "peak_rss_kib": ("max", 110),
}
PARAMETERS = {"samples": 100, "warmup": 3, "trials": 3, "workers": 2,
              "sizes": list(SIZES), "batches": list(BATCHES)}
KEY = ("profile", "implementation", "op", "block", "batch", "layout")
CASES = {(profile, impl, op, *shape) for profile, implementations in PROFILES.items()
         for impl in implementations for op in ("read", "write") for shape in workload_shapes(SIZES, BATCHES)}
RAW_FIELDS = ("verified", "payload_bytes", "samples_ns", "cpu_ns", "direct_io", "physical_owners", "process_threads")


def require(ok, message):
    if not ok:
        raise ValueError(message)


def trial_resources(directory, row, commands, executables):
    profile, impl, op, block, batch, layout = key = tuple(row[k] for k in KEY)
    trial = row["trial"]
    require(key in CASES and type(block) is int and type(batch) is int
            and type(trial) is int and 0 <= trial < PARAMETERS["trials"], f"unexpected workload or trial: {key}, {trial}")
    name = f"{trial}-{impl}-{profile}-{op}-{block}-{batch}-{layout}"
    invocation = commands[name]
    argv = invocation["command"]
    backend = "io_uring" if profile.startswith("io_uring-") else profile
    expected = ["/usr/bin/time", "-f", "\\nORBITE_PEAK_RSS_KIB=%M", "--", executables[impl],
                backend, op, str(block), str(batch), layout, "3", "100", "2"]
    require(argv[:-1] == expected and Path(argv[-1]).name == "orbite-bench-"+name, f"{name}: wrong measurement command")
    env = {"MC_RPC_CLIENT_IO_THREADS": "2", "MC_TE_RPC_CLIENT_IO_THREADS": "2"} if impl == "tent" else {}
    require(invocation["env_overrides"] == env, f"{name}: unexpected measurement settings")
    raw = json.loads((directory / (name + ".stdout")).read_text())
    require(all(raw[k] == row[k] for k in RAW_FIELDS), f"{name}: row differs from child stdout")
    rss = peak_rss((directory / (name + ".stderr")).read_text())
    require(type(row.get("peak_rss_kib")) is int and row["peak_rss_kib"] == rss, f"{name}: peak RSS differs")
    require(type(row["process_threads"]) is int and row["process_threads"] > 0, f"{name}: missing thread count")
    file = profile.startswith("io_uring-")
    if impl == "orbite":
        owners = 1 if profile != "cpu" and not file and layout == "contiguous" else batch
        require(type(row["physical_owners"]) is int and row["physical_owners"] == owners, f"{name}: wrong owner count")
        if file:
            require(row["direct_io"] is (profile == "io_uring-ext4"), f"{name}: wrong file route")
    samples = row["samples_ns"]
    total_bytes = block * batch * len(samples)
    return {"goodput_GiB_s": Fraction(total_bytes * 10**9, sum(samples) * 2**30),
            "p99_ns": sorted(samples)[(99 * len(samples) + 99) // 100 - 1],
            "cpu_ns_per_byte": Fraction(sum(row["cpu_ns"]), total_bytes), "peak_rss_kib": rss}


def check_settings(path, manifest):
    require(manifest.get("status") == "passed", "incomplete benchmark run")
    require(manifest.get("peak_rss") == RSS_METHOD, "missing per-process peak RSS measurement method")
    require(manifest["dirty"] is False and manifest["measurement_dirty"] is False, "dirty benchmark source")
    require(re.fullmatch(r"[0-9a-f]{40}", manifest["orbite_revision"]), "invalid OrbiTE revision")
    for key in ("tent", "orbite"):
        require(re.fullmatch(r"[0-9a-f]{64}", manifest["binary_sha256"][key]), f"invalid {key} binary digest")
    require(manifest["tent_revision"] == CONTRACT["tent_revision"], "wrong frozen TENT revision")
    for name, expected in (("orbite-revision", manifest["orbite_revision"]), ("orbite-status", ""),
                           ("tent-revision", manifest["tent_revision"]), ("cpu", manifest["cpu"]),
                           ("rustc", manifest["rustc"]), ("cxx", manifest["cxx"])):
        require((path / (name + ".stdout")).read_text().strip() == expected.strip(),
                f"{name}: manifest differs from command receipt")
    for field in ("boot_id", "sweep_id"):
        require(str(uuid.UUID(manifest[field])) == manifest[field], f"invalid {field}")
    for fs in ("ext4", "tmpfs"):
        mount = manifest["mounts"][fs]
        require(len(mount.split()) >= 4 and mount.split()[1] == fs, f"wrong {fs} filesystem")
        require((path / (fs + "-fs.stdout")).read_text().strip() == fs and
                (path / (fs + "-mount.stdout")).read_text().strip() == mount, f"{fs}: mount receipt differs")
    require(manifest["guard_negative_controls"] == 2 and manifest["reporter_negative_controls"] == 5,
            "missing correctness controls")
    require(all(manifest["parameters"][k] == v for k, v in PARAMETERS.items()), "nonstandard workload parameters")
    for field, expected in (("file_state", FILE_STATE), ("verification_io", VERIFICATION_IO), ("poll", POLL),
                            ("timing", TIMING), ("checks", CHECKS), ("tent_tcp_local_retry", TENT_RETRY),
                            ("tent_rpc_pools", {"MC_RPC_CLIENT_IO_THREADS": "2", "MC_TE_RPC_CLIENT_IO_THREADS": "2"}),
                            ("builds", {"rust": "release, io-uring", "tent": CONTRACT["cmake_options"]})):
        require(manifest[field] == expected, f"nonstandard {field}")
    require(not any(manifest["parameters"][k] for k in ("profile", "profile_only", "tail_profile_only")),
            "diagnostic run cannot qualify performance")
    require(manifest["workers_per_engine"] == 2 and manifest["caller_threads"] == 1 and manifest["merge"] is True
            and manifest["metrics"] is False and manifest["engine_max_failover_attempts"] == 0,
            "nonstandard engine configuration")


def formal_sweep(path):
    manifest = json.loads((path / "manifest.json").read_text())
    check_settings(path, manifest)
    invocations = json.loads((path / "commands.json").read_text())
    commands = {record["name"]: record for record in invocations}
    require(len(invocations) == len(commands), "duplicate command receipt")
    built = [json.loads(line) for line in (path / "rust-build.stdout").read_text().splitlines()]
    rust = [r["executable"] for r in built if r.get("executable") and r.get("target", {}).get("name") == "transfer"]
    tent_build = commands["tent-build"]["command"]
    require(len(rust) == 1 and tent_build[-2] == "-o", "missing benchmark executable identity")
    executables = {"orbite": rust[0], "tent": tent_build[-1]}
    _, identities, rows = load(path, manifest=manifest,
                               row_metrics=lambda directory, row: trial_resources(directory, row, commands, executables))
    require(set(rows) == CASES and len(identities) == len(CASES) * PARAMETERS["trials"], "incomplete CPU matrix")
    return manifest, rows


def compare_trials(candidate, baseline):
    result = {}
    for metric, (direction, percent) in LIMITS.items():
        current = statistics.median(row[metric] for row in candidate)
        reference = statistics.median(row[metric] for row in baseline)
        ratio = Fraction(current) / reference
        threshold = Fraction(percent, 100)
        passed = ratio >= threshold if direction == "min" else ratio <= threshold
        result[metric] = {"candidate": float(current), "baseline": float(reference),
                          "ratio": float(ratio), "within_limit": passed}
    return result


def evaluate(candidate, baseline=None):
    manifest, rows = formal_sweep(candidate)
    anchor_rows = {}
    if baseline is not None:
        require(not candidate.samefile(baseline), "baseline needs a separate sweep")
        anchor, anchor_rows = formal_sweep(baseline)
        require(anchor["orbite_revision"] == ORBITE_BASELINE, "wrong OrbiTE baseline revision")
        require(anchor["sweep_id"] != manifest["sweep_id"], "baseline needs an independent sweep")
        match_runs(anchor, manifest)
        for field in ("boot_id", "mounts", "peak_rss"):
            require(anchor[field] == manifest[field], f"unmatched {field}")
        require(anchor["harness_sha256"]["run.py"] == manifest["harness_sha256"]["run.py"],
                "different measurement drivers")
        require(anchor["binary_sha256"]["tent"] == manifest["binary_sha256"]["tent"], "different frozen TENT binaries")
    revision_miss = "same-revision-variation" if manifest["orbite_revision"] == ORBITE_BASELINE else "regression"
    verdicts = []
    for key in sorted(CASES):
        profile, impl, op, block, batch, layout = key
        if impl != "orbite":
            continue
        references = [("orbite_baseline", anchor_rows.get(key), revision_miss)]
        if "tent" in PROFILES[profile]:
            references.append(("frozen_tent", rows[profile, "tent", op, block, batch, layout], "below-reference-target"))
        for reference, trials, miss in references:
            metrics = compare_trials(rows[key], trials) if trials else {}
            status = "baseline-missing"
            if metrics:
                status = "within-limits" if all(v["within_limit"] for v in metrics.values()) else miss
            verdicts.append({"profile": profile, "op": op, "block": block, "batch": batch, "layout": layout,
                             "reference": reference, "status": status, "metrics": metrics})
    counts = collections.Counter(v["status"] for v in verdicts)
    status = "passed"
    if counts["baseline-missing"]:
        status = "incomplete"
    elif counts["within-limits"] != len(verdicts):
        status = "outside-limits"
    return {"policy_version": 1, "status": status, "candidate": manifest["orbite_revision"],
            "orbite_baseline": ORBITE_BASELINE, "tent_revision": CONTRACT["tent_revision"],
            "host": manifest["host"], "boot_id": manifest["boot_id"], "rustc": manifest["rustc"],
            "limits_percent_of_baseline": LIMITS, "counts": counts, "workloads": verdicts}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("candidate", type=Path)
    parser.add_argument("--baseline", type=Path, help="measured frozen OrbiTE checkout on the same Linux boot")
    parser.add_argument("--output", type=Path, help="write all workload verdicts as JSON")
    parser.add_argument("--report-only", action="store_true", help="record target misses and an absent optional baseline without failing the measurement job")
    args = parser.parse_args()
    try:
        report = evaluate(args.candidate, args.baseline)
    except (OSError, ValueError, KeyError, IndexError, TypeError, AttributeError) as error:
        report = {"policy_version": 1, "status": "invalid-evidence", "error": str(error)}
    content = json.dumps(report, indent=2, allow_nan=False) + "\n"
    if args.output:
        args.output.write_text(content)
    else:
        sys.stdout.write(content)
    print(f"CPU workload guards: {report['status']} {report.get('error', dict(report.get('counts', {})))}", file=sys.stderr)
    if report["status"] == "invalid-evidence":
        return 2
    if args.report_only or report["status"] == "passed":
        return 0
    return 2 if report["status"] == "incomplete" else 1


if __name__ == "__main__":
    sys.exit(main())
