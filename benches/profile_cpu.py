"""Profile the exact S26 executables; traced timings are never benchmark results."""
import json
from pathlib import Path
import re
import shutil
import subprocess


def syscall_rows(text):
    rows = {}
    for line in text.splitlines():
        fields = line.split()
        if len(fields) not in (5, 6) or fields[-1] == "total" or not re.fullmatch(r"[a-z][a-z0-9_]+", fields[-1]):
            continue
        try:
            rows[fields[-1]] = {"seconds": float(fields[1]), "calls": int(fields[3]),
                                "errors": int(fields[4]) if len(fields) == 6 else 0}
        except ValueError:
            continue
    if not rows:
        raise ValueError("empty strace summary")
    return rows


def record(out, executables, args, env, tent_env):
    """Count syscalls with a setup control, then sample user CPU call stacks."""
    if not shutil.which("strace"):
        raise RuntimeError("profiling requires strace")
    perf = sorted(Path("/usr/lib/linux-tools").glob("*/perf"))
    perf = str(perf[-1]) if perf else shutil.which("perf")
    if not perf:
        raise RuntimeError("profiling requires perf (software cpu-clock event)")
    root = out / "profiles"
    root.mkdir()
    commands, records = [], []

    def run(name, command, overrides):
        command = list(map(str, command))
        commands.append({"name": name, "command": command, "env_overrides": overrides})
        (root / "commands.json").write_text(json.dumps(commands, indent=2) + "\n")
        with (root / (name + ".stdout")).open("w") as stdout, (root / (name + ".stderr")).open("w") as stderr:
            subprocess.run(command, env=env | overrides, stdout=stdout, stderr=stderr,
                           check=True, timeout=600)
        return root / (name + ".stdout")

    def verified(path, samples):
        value = json.loads(path.read_text())
        if value.get("verified") is not True or len(value["samples_ns"]) != samples:
            raise ValueError("profile did not finish every byte check")

    shapes = [("tcp", 1, "contiguous"), ("tcp", 64, "gapped"),
              ("io_uring-tmpfs", 1, "contiguous"), ("io_uring-ext4", 64, "gapped")]
    for profile, batch, layout in shapes:
        backend = "io_uring" if profile.startswith("io_uring") else profile
        directory = args.ext4 if profile.endswith("ext4") else args.tmpfs
        for op in ("read", "write"):
            for implementation, exe in executables:
                name = f"{implementation}-{profile}-{op}-{batch}"
                path = directory.resolve() / ("orbite-profile-" + name)
                if path.exists():
                    raise FileExistsError(path)
                overrides = tent_env if implementation == "tent" else {}

                def command(samples):
                    return [exe, backend, op, 4096, batch, layout, args.warmup,
                            samples, args.workers, path]

                try:
                    summaries = []
                    for count, label in ((1, "setup"), (args.samples, "measured")):
                        trace = root / (name + "-" + label + ".strace")
                        output = run(name + "-" + label,
                                     ["strace", "-f", "-c", "-w", "-o", trace, *command(count)], overrides)
                        verified(output, count)
                        summaries.append(syscall_rows(trace.read_text()))
                        path.unlink(missing_ok=True)
                    setup, measured = summaries
                    delta = args.samples - 1
                    if delta <= 0:
                        raise ValueError("profiling needs at least two samples")
                    rates = {key: {"calls_per_batch": (value["calls"] - setup.get(key, {}).get("calls", 0)) / delta,
                                   "wall_us_per_batch": (value["seconds"] - setup.get(key, {}).get("seconds", 0)) * 1e6 / delta}
                             for key, value in measured.items()}
                    data = root / (name + ".perf")
                    # Short-lived batch threads can finish before a 1 ms sample.
                    # These high-rate stack samples are diagnostic, never timed results.
                    cpu_samples = max(args.samples, 1000)
                    output = run(name + "-cpu", [perf, "record", "-q", "-e", "cpu-clock:u", "-c", "10000",
                                                  "--call-graph", "dwarf,8192", "-o", data, "--", *command(cpu_samples)], overrides)
                    verified(output, cpu_samples)
                    path.unlink(missing_ok=True)
                    run(name + "-frames", [perf, "report", "--stdio", "--no-children", "--percent-limit", "0.5", "-i", data], {})
                    output = run(name + "-switches", [perf, "stat", "-x,", "-e", "task-clock,context-switches,cpu-migrations",
                                                       "--", *command(args.samples)], overrides)
                    verified(output, args.samples)
                    records.append({"implementation": implementation, "profile": profile, "op": op,
                                    "block": 4096, "batch": batch, "layout": layout,
                                    "warmup": args.warmup, "samples": args.samples, "cpu_samples": cpu_samples,
                                    "syscalls": rates, "verified": True})
                    (root / "summary.json").write_text(json.dumps(records, indent=2) + "\n")
                    print(json.dumps(records[-1]), flush=True)
                finally:
                    path.unlink(missing_ok=True)
    return len(records)
