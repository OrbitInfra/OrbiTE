"""Matched, verified CPU measurements. Evidence MUST be outside the checkout."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import statistics
import subprocess
import sys
import uuid

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "tests/reference"))
try:
    from manifest import CONTRACT, load_manifest, validate as validate_cpu_manifest
finally:
    # The oracle has its own run.py. Subsequent benchmark imports must resolve here.
    sys.path.pop(0)

SIZES = (4096, 65536, 1048576, 67108864)
BATCHES = (1, 8, 64)
PROFILES = {
    "cpu": ("orbite", "tent"), "tcp": ("orbite", "tent"),
    "hp_tcp": ("orbite",), "shm": ("orbite",),
    "io_uring-ext4": ("orbite", "tent"), "io_uring-tmpfs": ("orbite", "tent"),
}
RSS_PREFIX = "ORBITE_PEAK_RSS_KIB="
RSS_METHOD = "GNU time %M, Linux KiB, entire fresh benchmark process"
POLL = "128 hot then 1,2,4,...,128,200 us capped"
TIMING = "allocate batch + submit + poll + free; per-batch wall and whole-process CPU"
CHECKS = "every warmup and sample; changed pattern, reset destination, full bytes and guards; excluded from time"
FILE_STATE = "preallocated once; in-place pwrite + fdatasync before each sample; warmed ext4 O_DIRECT / tmpfs; timed writes are not durability receipts"
VERIFICATION_IO = "whole-file bulk read in both harnesses"
TENT_RETRY = {"max_retry_count": 3, "base_delay_ms": 100, "max_delay_ms": 2000}
RUST_BUILD = ["cargo", "build", "--release", "--locked", "--features", "io-uring", "--bench", "transfer", "--message-format=json"]


def tent_build_command(reference, harness, output):
    include = reference / "reference/mooncake-transfer-engine/tent/include"
    library = reference / "tent-build/mooncake-transfer-engine/tent/src/libtent_shared.so"
    return ["c++", "-std=c++20", "-O3", "-DNDEBUG", "-Wall", "-Wextra", "-Werror", "-I", str(include),
            str(harness / "benches/tent.cpp"), str(library), "-Wl,-rpath,"+str(library.parent), "-o", str(output / "tent-bench")]


def workload_shapes(sizes, batches):
    return ([(size, 1, "contiguous") for size in sizes]
            + [(4096, batch, layout) for batch in batches if batch != 1 for layout in ("contiguous", "gapped")])


def peak_rss(stderr):
    receipts = [line.removeprefix(RSS_PREFIX) for line in stderr.splitlines() if line.startswith(RSS_PREFIX)]
    if len(receipts) != 1 or not receipts[0].isascii() or not receipts[0].isdecimal() or int(receipts[0]) <= 0:
        raise ValueError("missing or invalid per-process peak RSS receipt")
    return int(receipts[0])


def nearest_rank_p99(values):
    return sorted(values)[(99 * len(values) + 99) // 100 - 1]


def check_caller_dep_info(text, executable, caller):
    # rustc emits one Make rule for the linked executable and a phony rule
    # for each input. This standalone caller has a single source file.
    source = str(caller).replace(" ", "\\ ")
    targets = {str(executable), str(executable).replace(" ", "\\ ")}
    lines = text.splitlines()
    if not any(f"{target}: {source}" in lines for target in targets) or f"{source}:" not in lines:
        raise ValueError("compiled benchmark does not depend on the measured caller")


def validate_samples(row, count, payload):
    if row.get("verified") is not True or row.get("payload_bytes") != payload:
        raise ValueError("unverified or wrong-payload result")
    for key in ("samples_ns", "cpu_ns"):
        values = row[key]
        if len(values) != count or any(type(n) is not int or n <= 0 for n in values):
            raise ValueError("incomplete or invalid measurements")


def summarize(row, count, payload):
    validate_samples(row, count, payload)
    latency = row["samples_ns"]
    return {"GiB_s": payload * count / sum(latency) * 1e9 / 2**30,
            "p50_us": statistics.median(latency) / 1000,
            "p99_us": nearest_rank_p99(latency) / 1000,
            "cpu_us_per_batch": statistics.mean(row["cpu_ns"]) / 1000}


def controls():
    valid = {"verified": True, "payload_bytes": 4096, "samples_ns": [10, 20], "cpu_ns": [10, 20]}
    summarize(valid, 2, 4096)
    for mutation in ({"verified": False}, {"payload_bytes": 1}, {"samples_ns": [10]},
                     {"cpu_ns": [0, 1]}, {"samples_ns": [float("nan"), 1]}):
        try:
            summarize(valid | mutation, 2, 4096)
        except ValueError:
            continue
        raise AssertionError("reporter accepted a broken measurement")


def parse_args(argv=None):
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--reference", type=Path, required=True, help="successful tests/reference/run.py output, built with liburing-dev installed")
    p.add_argument("--output", type=Path, required=True)
    p.add_argument("--orbite-source", type=Path, default=ROOT, help="build the Rust benchmark from this checkout with the current measurement driver")
    p.add_argument("--ext4", type=Path, required=True)
    p.add_argument("--tmpfs", type=Path, default=Path("/dev/shm"))
    p.add_argument("--samples", type=int)
    p.add_argument("--warmup", type=int, default=3)
    p.add_argument("--workers", type=int, default=2)
    p.add_argument("--sizes", type=int, nargs="+")
    p.add_argument("--batches", type=int, nargs="+")
    p.add_argument("--trials", type=int)
    p.add_argument("--profile", action="store_true", help="also collect strace/perf evidence for the slow-path targets")
    p.add_argument("--profile-only", action="store_true", help="collect profiles without reporting traced timings as benchmark rows")
    p.add_argument("--tail-profile-only", nargs="?", const="all", default=False,
                   choices=("all", "tmpfs-read-batch", "ext4-read-batch"),
                   help="trace the full suite or eight contiguous file READs on one filesystem")
    p.add_argument("--tmpfs-tail-baseline", type=Path, help="compare singleton tmpfs tails in balanced A/A, A/B and TENT runs using the frozen OrbiTE checkout")
    rss = p.add_mutually_exclusive_group()
    rss.add_argument("--tcp-read-rss-only", action="store_true", help="measure process RSS and heap allocations for a 64 MiB TCP READ")
    rss.add_argument("--tmpfs-read-rss-only", action="store_true", help="measure process RSS and heap allocations for a 64 MiB buffered tmpfs READ")
    a = p.parse_args(argv)
    focused = a.tmpfs_tail_baseline is not None
    measure_rss = a.tcp_read_rss_only or a.tmpfs_read_rss_only
    if a.samples is None:
        a.samples = 1000 if focused else 100
    if a.trials is None:
        a.trials = 12 if focused else 3
    if a.sizes is None:
        single_size = 64 << 20 if measure_rss else 4096
        a.sizes = [single_size] if focused or measure_rss else SIZES
    if a.batches is None:
        a.batches = [1] if focused or measure_rss else BATCHES
    a.output, a.reference, a.orbite_source = a.output.resolve(), a.reference.resolve(), a.orbite_source.resolve()
    out, ref, orbite = a.output, a.reference, a.orbite_source
    a.ext4, a.tmpfs = a.ext4.resolve(), a.tmpfs.resolve()
    if out.is_relative_to(ROOT) or out.is_relative_to(orbite) or out.is_relative_to(ref) or platform.system() != "Linux":
        p.error("Linux only; output must be outside both source trees")
    if not (1 <= a.samples <= 10000 and 1 <= a.warmup <= 100 and 1 <= a.workers <= 16 and 1 <= a.trials <= (12 if focused else 9)):
        p.error("invalid sample/worker bounds")
    if any(n < 4096 or n > 64 << 20 or n % 4096 for n in a.sizes) or any(n < 1 or n > 64 for n in a.batches):
        p.error("invalid sweep bounds")
    if focused and (
        a.profile or a.profile_only or a.tail_profile_only or orbite != ROOT
        or (a.warmup, a.samples, a.workers, a.trials, a.sizes, a.batches) != (3, 1000, 2, 12, [4096], [1])
        or out.is_relative_to(a.tmpfs_tail_baseline.resolve())
    ):
        p.error("tmpfs tail diagnostics require the current source, a separate output, 12 triplets per case, "
                "one 4 KiB block, 3 warmups, 1000 untraced samples and 2 workers")
    if measure_rss and (focused or a.profile or a.profile_only or a.tail_profile_only
            or (a.warmup, a.samples, a.workers, a.trials, a.sizes, a.batches) != (3, 100, 2, 3, [64 << 20], [1])):
        p.error("READ memory profiling requires one 64 MiB block, 3 warmups, 100 samples, 2 workers and 3 trials")
    return a


def main():
    a = parse_args()
    measure_rss = a.tcp_read_rss_only or a.tmpfs_read_rss_only
    out, ref, orbite = a.output, a.reference, a.orbite_source
    out.mkdir(parents=True, exist_ok=False)
    env = {k: v for k, v in os.environ.items()
           if not k.startswith(("MC_", "TENT_", "MOONCAKE_", "ORBITE_BENCH_"))
           and k not in ("LD_PRELOAD", "LD_LIBRARY_PATH")}
    commands = []
    tent_env = {"MC_RPC_CLIENT_IO_THREADS": str(a.workers), "MC_TE_RPC_CLIENT_IO_THREADS": str(a.workers)}

    def run(name, cmd, timeout=1200, overrides=None, expect_failure=False, cwd=ROOT):
        cmd = list(map(str, cmd))
        commands.append({"name": name, "command": cmd, "cwd": str(cwd), "env_overrides": overrides or {}})
        (out / "commands.json").write_text(json.dumps(commands, indent=2)+"\n")
        with (out / (name+".stderr")).open("w") as err:
            r = subprocess.run(cmd, cwd=cwd, env=env | (overrides or {}), text=True,
                               stdout=subprocess.PIPE, stderr=err, timeout=timeout)
        (out / (name+".stdout")).write_text(r.stdout)
        if (r.returncode == 0) == expect_failure:
            raise RuntimeError(f"unexpected exit {r.returncode}: {name}; see {out}")
        return r.stdout

    controls()
    source = ref / "reference"
    if run("tent-revision", ["git", "-C", source, "rev-parse", "HEAD"]).strip() != CONTRACT["tent_revision"]:
        raise ValueError("not the frozen TENT reference")
    oracle = load_manifest(ref / "manifest.json")
    validate_cpu_manifest(oracle, ref, "passed-cpu-trace")
    for path, expected in [(a.ext4, "ext4"), (a.tmpfs, "tmpfs")]:
        if run(expected+"-fs", ["findmnt", "-n", "-o", "FSTYPE", "-T", path]).strip() != expected:
            raise ValueError(f"{path} is not {expected}")
    def build_rust(name, checkout, overrides=None):
        declared = checkout / "benches/transfer.rs"
        caller = declared if measure_rss else ROOT / "benches/transfer.rs"
        redirected = caller != declared and caller.read_bytes() != declared.read_bytes()
        overrides = dict(overrides or {})
        if redirected:
            revision = run(name+"-engine", ["git", "rev-parse", "HEAD"], cwd=checkout).strip()
            digest = hashlib.sha256(caller.read_bytes()).hexdigest()
            builder = ROOT / "benches/build_caller.py"
            builder_digest = hashlib.sha256(builder.read_bytes()).hexdigest()
            cache = Path(overrides.get("CARGO_TARGET_DIR") or env.get("CARGO_TARGET_DIR") or (out.parent / "cargo"))
            overrides.update(RUSTC_WORKSPACE_WRAPPER=str(builder),
                             CARGO_TARGET_DIR=str(cache.resolve() / builder_digest / "bench-callers" / revision / digest))
            if inner := env.get("RUSTC_WORKSPACE_WRAPPER"):
                overrides["ORBITE_BENCH_INNER_WRAPPER"] = inner
        built = run(name, RUST_BUILD, cwd=checkout, overrides=overrides)
        exe, = [Path(r["executable"]) for line in built.splitlines() if (r := json.loads(line)).get("executable") and r.get("target", {}).get("name") == "transfer"]
        if redirected:
            deps = run(name+"-deps", ["cat", exe.with_suffix(".d")])
            check_caller_dep_info(deps, exe, caller)
            run(name+"-caller", ["sha256sum", caller])
        return exe

    rust = build_rust("rust-build", orbite)
    tent = out / "tent-bench"
    tent_build = tent_build_command(ref, ROOT, out)
    if measure_rss:
        tent_build.insert(1, "-g1")
    run("tent-build", tent_build)
    harness_files = [f for f in (ROOT / "benches").iterdir() if f.is_file() and f.name != "transfer.rs"]
    harness_files.append((orbite if measure_rss else ROOT) / "benches/transfer.rs")
    manifest = {"orbite_revision": run("orbite-revision", ["git", "rev-parse", "HEAD"], cwd=orbite).strip(),
                "checkout_caller_sha256": hashlib.sha256((orbite / "benches/transfer.rs").read_bytes()).hexdigest(),
                "dirty": bool(run("orbite-status", ["git", "status", "--porcelain"], cwd=orbite).strip()),
                "measurement_dirty": bool(subprocess.check_output(["git", "status", "--porcelain"], cwd=ROOT)),
                "boot_id": Path("/proc/sys/kernel/random/boot_id").read_text().strip(),
                "sweep_id": str(uuid.uuid4()),
                "mounts": {name: run(name+"-mount", ["findmnt", "-n", "-o", "SOURCE,FSTYPE,MAJ:MIN,OPTIONS", "-T", path]).strip()
                           for name, path in (("ext4", a.ext4), ("tmpfs", a.tmpfs))},
                "peak_rss": RSS_METHOD,
                "tent_revision": CONTRACT["tent_revision"], "host": platform.node(), "kernel": platform.platform(),
                "cpu": run("cpu", ["lscpu"]), "affinity": sorted(os.sched_getaffinity(0)),
                "rustc": run("rustc", ["rustc", "-Vv"]), "cxx": run("cxx", ["c++", "--version"]),
                "parameters": {k: str(v) if isinstance(v, Path) else v for k, v in vars(a).items()},
                "caller_threads": 1, "workers_per_engine": a.workers, "tent_rpc_pools": tent_env, "merge": True,
                "engine_max_failover_attempts": 0,
                "tent_tcp_local_retry": TENT_RETRY, "poll": POLL, "metrics": False,
                "timing": TIMING, "checks": CHECKS, "file_state": FILE_STATE, "verification_io": VERIFICATION_IO,
                "builds": {"rust": "release, io-uring", "tent": oracle["contract"]["cmake_options"]},
                "harness_sha256": {f.name: hashlib.sha256(f.read_bytes()).hexdigest() for f in harness_files},
                "binary_sha256": {k: hashlib.sha256(v.read_bytes()).hexdigest() for k, v in {"orbite": rust, "tent": tent}.items()},
                "status": "running"}
    (out / "manifest.json").write_text(json.dumps(manifest, indent=2)+"\n")
    for name, exe in [("orbite", rust), ("tent", tent)]:
        text = run(name+"-bad-byte", [exe, "tcp", "read", 4096, 1, "contiguous", 1, 1, a.workers, "-"], overrides=({**(tent_env if name == "tent" else {}), "ORBITE_BENCH_CORRUPT_BYTE": "1"}), expect_failure=True)
        if '"verified":true' in text:
            raise ValueError("corrupted guard produced a report")
    if measure_rss:
        from profile_read_rss import measure
        if manifest["dirty"] or manifest["measurement_dirty"]:
            raise ValueError("READ memory comparisons require clean engine and measurement checkouts")
        measure(out, rust, tent, run, tent_env, tmpfs=a.tmpfs if a.tmpfs_read_rss_only else None)
        profile = "tmpfs" if a.tmpfs_read_rss_only else "tcp"
        manifest["status"] = f"completed-{profile}-read-memory-profile"
        (out / "manifest.json").write_text(json.dumps(manifest, indent=2)+"\n")
        return
    if a.tmpfs_tail_baseline:
        from guards import ORBITE_BASELINE
        from tmpfs_tail import measure
        base = a.tmpfs_tail_baseline.resolve()
        revision = run("baseline-revision", ["git", "rev-parse", "HEAD"], cwd=base).strip()
        if (revision != ORBITE_BASELINE or run("baseline-status", ["git", "status", "--porcelain"], cwd=base).strip()
                or manifest["dirty"] or manifest["measurement_dirty"]):
            raise ValueError("tmpfs tail diagnostics require clean candidate and frozen baseline revisions")
        frozen = build_rust("baseline-build", base, {"CARGO_TARGET_DIR": str(out / "baseline-target")})
        manifest["baseline_revision"] = revision
        manifest["binary_sha256"]["baseline"] = hashlib.sha256(frozen.read_bytes()).hexdigest()
        measure(out, a.tmpfs, run, frozen, rust, tent, tent_env)
        manifest["status"] = "completed-tmpfs-tail-diagnostic"
        (out / "manifest.json").write_text(json.dumps(manifest, indent=2)+"\n")
        return
    if a.tail_profile_only:
        from profile_io_tail import record
        manifest["profile_cases"] = record(out, [("orbite", rust), ("tent", tent)], a, env, tent_env)
        manifest["status"] = "passed-tail-profiles"
        (out / "manifest.json").write_text(json.dumps(manifest, indent=2)+"\n")
        return
    if a.profile or a.profile_only:
        from profile_cpu import record
        manifest["profile_cases"] = record(out, [("orbite", rust), ("tent", tent)], a, env, tent_env)
        if a.profile_only:
            manifest["status"] = "passed-profiles"
            (out / "manifest.json").write_text(json.dumps(manifest, indent=2)+"\n")
            return
    shapes = workload_shapes(a.sizes, a.batches)
    rows = []
    for trial in range(a.trials):
        for profile, names in PROFILES.items():
            backend = "io_uring" if profile.startswith("io_uring") else profile
            implementations = [(name, rust if name == "orbite" else tent) for name in names]
            if trial % 2: implementations.reverse()
            for op in ("read", "write"):
                for block, batch, layout in shapes:
                    for implementation, exe in implementations:
                        name = f"{trial}-{implementation}-{profile}-{op}-{block}-{batch}-{layout}"
                        directory = a.ext4 if profile.endswith("ext4") else a.tmpfs
                        path = directory.resolve() / ("orbite-bench-"+name)
                        if path.exists(): raise FileExistsError(path)
                        try:
                            text = run(name, ["/usr/bin/time", "-f", "\\n"+RSS_PREFIX+"%M", "--",
                                             exe, backend, op, block, batch, layout, a.warmup, a.samples, a.workers, path],
                                       overrides=tent_env if implementation == "tent" else None)
                            row = json.loads(text)
                            row["peak_rss_kib"] = peak_rss((out / (name+".stderr")).read_text())
                            summary = summarize(row, a.samples, block*batch)
                            if implementation == "orbite" and backend == "io_uring" and row["direct_io"] != profile.endswith("ext4"):
                                raise ValueError("wrong file route")
                            expected_owners = 1 if backend != "io_uring" and layout == "contiguous" else batch
                            if implementation == "orbite" and backend != "cpu" and row["physical_owners"] != expected_owners:
                                raise ValueError("merge/coalescing path did not match layout")
                            record = dict(trial=trial, implementation=implementation, profile=profile, op=op, block=block, batch=batch, layout=layout, **row, **summary)
                            rows.append(record)
                            with (out / "results.jsonl").open("a") as f: f.write(json.dumps(record)+"\n")
                            print(json.dumps({k: v for k, v in record.items() if k not in ("samples_ns", "cpu_ns")}), flush=True)
                        finally:
                            path.unlink(missing_ok=True)
    for backend in ("GPU", "RDMA", "GDS"):
        with (out / "results.jsonl").open("a") as f: f.write(json.dumps({"profile": backend, "status": "not run (hardware)"})+"\n")
    manifest.update(status="passed", verified_rows=len(rows), reporter_negative_controls=5, guard_negative_controls=2)
    (out / "manifest.json").write_text(json.dumps(manifest, indent=2)+"\n")


if __name__ == "__main__":
    main()
