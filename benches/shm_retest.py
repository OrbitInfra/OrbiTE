"""Focused, untraced fixed/fixed and fixed/main SHM trial. See the ordinary sweep for formal gates."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
from statistics import median
import subprocess
import sys

SAMPLES = 1000
PAYLOAD = 4096 * 8
HORIZONS = (100, SAMPLES)


def schedule(replica):
    pairs = []
    for block in range(12):
        cases = ("AA", "AB") if block % 2 == 0 else ("AB", "AA")
        for case in cases:
            # Each four-block group covers every combination of pair position and role order.
            baseline_first = (block % 4 in (0, 1)) == (case == "AA")
            roles = ("baseline", "probe") if baseline_first else ("probe", "baseline")
            pairs.append((block, case, roles))
    if replica == 2:
        pairs = [(block, case, roles[::-1]) for block, case, roles in reversed(pairs)]
    return pairs


def json_file(path, data):
    path.write_text(json.dumps(data, indent=2) + "\n")


def read_metrics(raw, stderr, bench, samples=SAMPLES):
    bench.validate_samples(raw, samples, PAYLOAD)
    if (type(raw.get("physical_owners")) is not int or raw["physical_owners"] != 1
            or "direct_io" not in raw or raw["direct_io"] is not None
            or type(raw.get("process_threads")) is not int or raw["process_threads"] < 1):
        raise ValueError("missing SHM owner, route or thread receipt")
    rss = bench.peak_rss(stderr)
    metrics = {}
    for count in (n for n in HORIZONS if n <= samples):
        part = dict(raw, samples_ns=raw["samples_ns"][:count], cpu_ns=raw["cpu_ns"][:count])
        stats = bench.summarize(part, count, PAYLOAD)
        metrics[str(count)] = {
            "p99_ns": bench.nearest_rank_p99(part["samples_ns"]),
            "goodput_GiB_s": stats["GiB_s"],
            "cpu_ns_per_byte": sum(part["cpu_ns"]) / (PAYLOAD * count),
            "peak_rss_kib": rss,
        }
    return metrics


def summarize(pairs, limits):
    report = {}
    for case in ("AA", "AB"):
        rows = [row for row in pairs if row["case"] == case]
        if len(rows) != 12 or {r["block"] for r in rows} != set(range(12)):
            raise ValueError(f"incomplete pairs: {case}")
        groups = {role: [r for r in rows if r["order"][0] == role]
                  for role in ("baseline", "probe")}
        if any(len(group) != 6 for group in groups.values()):
            raise ValueError(f"unbalanced order: {case}")
        by_horizon = {}
        for horizon in map(str, HORIZONS):
            metrics = {}
            for name, (side, percent) in limits.items():
                def values(group, role):
                    return [r["processes"][role]["metrics"][horizon][name] for r in group]

                def ratios(group):
                    return [probe / base for base, probe in zip(values(group, "baseline"),
                                                                values(group, "probe"))]

                base, probe = (median(values(rows, role)) for role in ("baseline", "probe"))
                paired = ratios(rows)
                fails = [(p * 100 < b * percent if side == "min" else p * 100 > b * percent)
                         for b, p in zip(values(rows, "baseline"), values(rows, "probe"))]
                metrics[name] = {
                    "limits": [side, percent / 100], "baseline_median": base, "probe_median": probe,
                    "ratio_of_medians": probe / base, "paired_median_ratio": median(paired),
                    "pair_ratios": paired, "pairs_beyond_limit": sum(fails),
                    "median_ratio_by_first": {role: median(ratios(group)) for role, group in groups.items()},
                }
            by_horizon[horizon] = metrics
        report[case] = by_horizon
    return report


def trial(root, out, path, env, binaries, variant, stem, count, bench):
    cmd = ["/usr/bin/time", "-f", "\\n" + bench.RSS_PREFIX + "%M", "--", str(binaries[variant]),
           "shm", "read", "4096", "8", "contiguous", "3", str(count), "2", str(path)]
    with (out / (stem + ".stdout")).open("wb") as stdout, (out / (stem + ".stderr")).open("wb") as stderr:
        subprocess.run(cmd, cwd=root, env=env, stdout=stdout, stderr=stderr, timeout=300, check=True)
    raw = json.loads((out / (stem + ".stdout")).read_bytes())
    metrics = read_metrics(raw, (out / (stem + ".stderr")).read_text(), bench, count)
    return cmd, metrics


def standalone(root, out, path, env, binaries, bench, limits):
    # Match the formal sample and process counts for this one workload.
    readings = {}
    for variant in ("fixed", "main"):
        group = []
        for i in range(3):
            _, metrics = trial(root, out, path, env, binaries, variant, f"standalone-{variant}-{i}", 100, bench)
            group.append(metrics["100"])
        readings[variant] = group
    results = {}
    for name, (side, percent) in limits.items():
        base, candidate = (median(r[name] for r in readings[v]) for v in ("fixed", "main"))
        passes = candidate * 100 >= base * percent if side == "min" else candidate * 100 <= base * percent
        results[name] = dict(fixed=base, main=candidate, ratio=candidate / base, within_limit=passes)
    json_file(out / "standalone.json", {"trials": readings, "metrics": results})


def run(args):
    root, out = args.harness.resolve(), args.out.resolve()
    if out.is_relative_to(root):
        raise ValueError("place raw results outside the source checkout")
    sys.path.insert(0, str(root / "benches"))
    import run as bench
    from guards import LIMITS
    out.mkdir(parents=True, exist_ok=False)
    binaries = {"fixed": args.fixed.resolve(strict=True), "main": args.main.resolve(strict=True)}
    if binaries["fixed"] == binaries["main"]:
        raise ValueError("the AB pair needs independently built binaries")
    env = {key: val for key, val in os.environ.items()
           if not key.startswith(("ORBITE_", "MC_", "MOONCAKE_", "TENT_"))
           and key not in ("LD_PRELOAD", "LD_LIBRARY_PATH")}
    path = out / "unused-shm-file"
    mount = subprocess.check_output(["findmnt", "-n", "-o", "FSTYPE", "-T", "/dev/shm"], text=True).strip()
    if mount != "tmpfs":
        raise RuntimeError(f"/dev/shm must be tmpfs, found {mount}")
    manifest = {
        "study": "focused diagnostic; the first 100 samples are a subset of the same 1000",
        "workload": "shm read 4096 8 contiguous 3 1000 2",
        "compiler": args.compiler, "replica": args.replica, "kernel": platform.uname()._asdict(),
        "boot_id": Path("/proc/sys/kernel/random/boot_id").read_text().strip(), "shm_filesystem": mount,
        "cpu_affinity": sorted(os.sched_getaffinity(0)),
        "caller_sha256": hashlib.sha256((root / "benches/transfer.rs").read_bytes()).hexdigest(),
        "binaries": {name: {"path": str(p), "sha256": hashlib.sha256(p.read_bytes()).hexdigest()}
                     for name, p in binaries.items()},
    }
    json_file(out / "manifest.json", manifest)
    standalone(root, out, path, env, binaries, bench, LIMITS)
    pairs = []
    for block, case, roles in schedule(args.replica):
        row = {"block": block, "case": case, "order": roles, "processes": {}}
        for role in roles:
            variant = "main" if case == "AB" and role == "probe" else "fixed"
            stem = f"{block:02}-{case}-{role}"
            cmd, metrics = trial(root, out, path, env, binaries, variant, stem, SAMPLES, bench)
            row["processes"][role] = {"variant": variant, "command": cmd, "raw_stem": stem, "metrics": metrics}
        pairs.append(row)
        json_file(out / "pairs.json", pairs)
    if path.exists():
        raise ValueError("SHM benchmark unexpectedly created a file at the file workload path")
    results = summarize(pairs, LIMITS)
    json_file(out / "summary.json", results)
    for case, horizons in results.items():
        for horizon, metrics in horizons.items():
            p99 = metrics["p99_ns"]
            print(case, horizon, "p99 paired median", p99["paired_median_ratio"],
                  "crossings", p99["pairs_beyond_limit"], "of 12", flush=True)


if __name__ == "__main__":
    cli = argparse.ArgumentParser(description=__doc__)
    for option in ("harness", "fixed", "main", "out"):
        cli.add_argument("--" + option, type=Path, required=True)
    cli.add_argument("--replica", type=int, choices=(1, 2), required=True)
    cli.add_argument("--compiler", required=True)
    run(cli.parse_args())
