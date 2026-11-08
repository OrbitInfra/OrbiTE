"""Render every matched S26 case; raw data and output stay outside Git until review."""
import argparse
import collections
import json
from pathlib import Path
import statistics

from run import summarize


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
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("before", type=Path)
    parser.add_argument("after", type=Path)
    parser.add_argument("--changed-profile", action="append", help="OrbiTE profile changed by this pair; repeat for several (default: tcp)")
    args = parser.parse_args()
    changed = args.changed_profile or ["tcp"]
    before, before_ids, old = load(args.before)
    after, after_ids, new = load(args.after)
    if before_ids != after_ids:
        raise ValueError("different workload rows")
    for key in ("host", "kernel", "cpu", "affinity", "rustc", "cxx", "tent_revision", "caller_threads", "workers_per_engine", "tent_rpc_pools", "merge", "engine_max_failover_attempts", "tent_tcp_local_retry", "poll", "metrics", "timing", "checks", "file_state", "builds"):
        if before[key] != after[key]:
            raise ValueError(f"unmatched {key}")
    for key in ("samples", "warmup", "workers", "trials", "sizes", "batches", "ext4", "tmpfs"):
        if before["parameters"][key] != after["parameters"][key]:
            raise ValueError(f"unmatched parameter {key}")
    for key in ("tent.cpp", "transfer.rs"):
        if before["harness_sha256"][key] != after["harness_sha256"][key]:
            raise ValueError(f"changed workload {key}")
    print(f"Before `{before['orbite_revision'][:8]}`; after `{after['orbite_revision'][:8]}`; "
          f"host `{after['host']}`; {len(after_ids)} verified rows on each side.")
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
        if profile not in changed or impl != "orbite":
            labels.append("U")
        print(f"| {profile} | {impl} | {op} | {block // 1024} KiB × {batch}, {layout} | "
              + " | ".join(cells) + " | " + (", ".join(labels) or "—") + " |")


if __name__ == "__main__":
    main()
