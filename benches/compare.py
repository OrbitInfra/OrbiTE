"""Render every matched S26 case; raw data and output stay outside Git until review."""
import argparse
import collections
import json
from pathlib import Path
import statistics

from run import summarize


def match_cpu(before, after):
    # This lscpu field samples the current clock, not the CPU configuration.
    # Keep the raw observations and report them; never normalize measured times.
    def split(text):
        observed = [line for line in text.splitlines() if line.startswith("CPU(s) scaling MHz:")]
        fixed = [line for line in text.splitlines() if line not in observed]
        return fixed, observed
    a, af = split(before)
    b, bf = split(after)
    if a != b:
        raise ValueError("unmatched cpu configuration")
    return (af, bf) if af != bf else None


def match_reset(before, after, reset_comparison):
    if reset_comparison:
        if (before, after) != (
            "buffered reset before each sample; warmed ext4 O_DIRECT / tmpfs; no fsync, no durability or SSD performance claim",
            "preallocated once; in-place pwrite + fdatasync before each sample; warmed ext4 O_DIRECT / tmpfs; timed writes are not durability receipts",
        ):
            raise ValueError("unexpected reset comparison")
    elif before != after:
        raise ValueError("unmatched file_state")


def match_verification(before, after, enabled):
    if enabled:
        if (before, after) != (None, "whole-file bulk read in both harnesses"):
            raise ValueError("unexpected verification comparison")
    elif before != after:
        raise ValueError("unmatched verification_io")


def match_harness(before, after, reset, verification):
    for key in ("tent.cpp", "transfer.rs"):
        if before[key] != after[key] and not reset and not (verification and key == "tent.cpp"):
            raise ValueError(f"changed workload {key}")


def controls():
    cpu = "Model name: example\nCPU(s): 4\nCPU max MHz: 2600\nCPU(s) scaling MHz: 116%\n"
    changed = cpu.replace("116%", "154%")
    if match_cpu(cpu, cpu) is not None or match_cpu(cpu, changed) is None:
        raise ValueError("CPU clock observation control failed")
    for old, new in [("example", "other"), ("CPU(s): 4", "CPU(s): 8"), ("2600", "3000")]:
        try:
            match_cpu(cpu, changed.replace(old, new))
        except ValueError:
            continue
        raise ValueError("CPU configuration mismatch was accepted")
    for enabled in (False, True):
        try:
            match_reset('unknown', 'different', enabled)
        except ValueError:
            continue
        raise ValueError("unknown reset regime was accepted")
    match_verification(None, "whole-file bulk read in both harnesses", True)
    for enabled in (False, True):
        try:
            match_verification(None, "unknown", enabled)
        except ValueError:
            continue
        raise ValueError("unknown verification regime was accepted")
    old = {"tent.cpp": "a", "transfer.rs": "b"}
    match_harness(old, old | {"tent.cpp": "c"}, False, True)
    try:
        match_harness(old, old | {"transfer.rs": "c"}, False, True)
    except ValueError:
        pass
    else:
        raise ValueError("verification comparison accepted a changed Rust workload")


def load(path):
    manifest = json.loads((path / "manifest.json").read_text())
    if manifest["status"] != "passed":
        raise ValueError("incomplete benchmark run")
    records, identities = collections.defaultdict(list), set()
    for line in (path / "results.jsonl").read_text().splitlines():
        row = json.loads(line)
        if row.get("status") == "not run (hardware)":
            continue
        key = tuple(row[k] for k in ("profile", "implementation", "op", "block", "batch", "layout"))
        identity = (row["trial"], *key)
        if identity in identities:
            raise ValueError("duplicate row")
        identities.add(identity)
        records[key].append(summarize(row, manifest["parameters"]["samples"], row["block"] * row["batch"]))
    if len(identities) != manifest["verified_rows"]:
        raise ValueError("missing verified rows")
    if any(len(v) != manifest["parameters"]["trials"] for v in records.values()):
        raise ValueError("missing trial")
    return manifest, identities, records


def main():
    controls()
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("before", type=Path)
    parser.add_argument("after", type=Path)
    parser.add_argument("--changed-profile", action="append", help="OrbiTE profile changed by this pair; repeat for several (default: tcp)")
    regimes = parser.add_mutually_exclusive_group()
    regimes.add_argument("--reset-comparison", action="store_true", help="explicit old truncate/reset versus preallocated pwrite/fdatasync harness comparison")
    regimes.add_argument("--verification-comparison", action="store_true", help="explicit C++ stream-iterator versus bulk file verification; reset and timed operations unchanged")
    args = parser.parse_args()
    changed = args.changed_profile or ["tcp"]
    before, before_ids, old = load(args.before)
    after, after_ids, new = load(args.after)
    if before_ids != after_ids:
        raise ValueError("different workload rows")
    clock_observation = match_cpu(before["cpu"], after["cpu"])
    match_reset(before["file_state"], after["file_state"], args.reset_comparison)
    match_verification(before.get("verification_io"), after.get("verification_io"), args.verification_comparison)
    for key in ("host", "kernel", "affinity", "rustc", "cxx", "tent_revision", "caller_threads", "workers_per_engine", "tent_rpc_pools", "merge", "engine_max_failover_attempts", "tent_tcp_local_retry", "poll", "metrics", "timing", "checks", "builds"):
        if before[key] != after[key]:
            raise ValueError(f"unmatched {key}")
    for key in ("samples", "warmup", "workers", "trials", "sizes", "batches", "ext4", "tmpfs"):
        if before["parameters"][key] != after["parameters"][key]:
            raise ValueError(f"unmatched parameter {key}")
    match_harness(before["harness_sha256"], after["harness_sha256"], args.reset_comparison, args.verification_comparison)
    print(f"Before `{before['orbite_revision'][:8]}`; after `{after['orbite_revision'][:8]}`; "
          f"host `{after['host']}`; {len(after_ids)} verified rows on each side.")
    if clock_observation:
        print(f"\nInstantaneous CPU clock observation changed: {clock_observation[0]} → {clock_observation[1]}. "
              "All other lscpu fields match. Frequency was not locked; timings are not frequency-normalized.")
    if args.reset_comparison:
        print("\nIntentional harness change in both implementations: truncate/buffered reset → preallocated in-place pwrite/fdatasync before timing. H marks those file rows; timed operations and byte checks are unchanged.")
    if args.verification_comparison:
        print("\nIntentional C++ harness change: file verification uses a whole-file bulk read, matching Rust. V marks those TENT file rows; production, reset, timed operations and byte checks are unchanged. Diagnostic pacing is disabled.")
    print("\nCells are before → after, median of the three trial statistics. All cases are included. "
          "R marks any numerical regression (p50/p99/CPU up or throughput down), not statistical significance; "
          "O means all four trial-statistic ranges overlap. U means the payload implementation is unchanged "
          "(shared-owner effects still require review). "
          "CPU is process time per batch. Raw trials remain in the linked artifact.\n")
    print("| Profile | Impl | Op | Block × batch, layout | p50 µs | p99 µs | CPU µs | GiB/s | Review |")
    print("|---|---|---|---|---:|---:|---:|---:|---|")
    for key in sorted(old):
        cells, regress, overlap = [], False, True
        for metric in ("p50_us", "p99_us", "cpu_us_per_batch", "GiB_s"):
            a, b = [v[metric] for v in old[key]], [v[metric] for v in new[key]]
            ma, mb = statistics.median(a), statistics.median(b)
            regress |= mb < ma if metric == "GiB_s" else mb > ma
            overlap &= max(min(a), min(b)) <= min(max(a), max(b))
            cells.append(f"{ma:.3f} → {mb:.3f}" if metric == "GiB_s" else f"{ma:.1f} → {mb:.1f}")
        profile, impl, op, block, batch, layout = key
        labels = (["R"] if regress else []) + (["O"] if overlap else [])
        if args.reset_comparison and profile.startswith('io_uring-'):
            labels.append("H")
        if args.verification_comparison and impl == 'tent' and profile.startswith('io_uring-'):
            labels.append("V")
        if profile not in changed or impl != "orbite":
            labels.append("U")
        print(f"| {profile} | {impl} | {op} | {block // 1024} KiB × {batch}, {layout} | "
              + " | ".join(cells) + " | " + (", ".join(labels) or "—") + " |")


if __name__ == "__main__":
    main()
