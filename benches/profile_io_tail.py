"""Bounded, diagnostic-only sample windows and kernel completion/scheduler traces."""
import json
from pathlib import Path
import shutil
import subprocess


def record(out, executables, args, env, tent_env):
    perf = sorted(Path("/usr/lib/linux-tools").glob("*/perf"))
    perf = str(perf[-1]) if perf else shutil.which("perf")
    if not perf:
        raise RuntimeError("tail profiling requires perf and root tracepoint access")
    root = out / "tail"
    root.mkdir()
    commands, records = [], []

    def run(name, command):
        command = list(map(str, command))
        commands.append({"name": name, "command": command})
        (root / "commands.json").write_text(json.dumps(commands, indent=2) + "\n")
        with (root / (name + ".stdout")).open("w") as stdout, (root / (name + ".stderr")).open("w") as stderr:
            subprocess.run(command, env=env, stdout=stdout, stderr=stderr, check=True, timeout=600)

    # Keep the kernel's field definitions with the evidence: names/layouts vary
    # across kernels. All-CPU recording includes io-wq and interrupt completions.
    run("formats", ["sudo", "sh", "-c", "cat /sys/kernel/tracing/events/io_uring/*/format"])
    events = ["io_uring:*", "sched:sched_switch", "sched:sched_wakeup",
              "sched:sched_process_fork", "sched:sched_process_exit",
              "syscalls:sys_enter_io_uring_setup", "syscalls:sys_exit_io_uring_setup",
              "syscalls:sys_enter_io_uring_enter", "syscalls:sys_exit_io_uring_enter",
              "syscalls:sys_enter_clock_nanosleep", "syscalls:sys_exit_clock_nanosleep",
              "syscalls:sys_enter_futex", "syscalls:sys_exit_futex"]
    shapes = [("ext4", 64, "gapped"), ("ext4", 8, "gapped"),
              ("ext4", 8, "contiguous"), ("ext4", 1, "contiguous"),
              ("tmpfs", 1, "contiguous")]
    samples = max(args.samples, 1000)
    for filesystem, batch, layout in shapes:
        for op in ("read", "write"):
            for implementation, exe in executables:
                name = f"{implementation}-{filesystem}-{op}-{batch}-{layout}"
                directory = args.ext4 if filesystem == "ext4" else args.tmpfs
                path = directory.resolve() / ("orbite-tail-" + name)
                if path.exists():
                    raise FileExistsError(path)
                data = root / (name + ".perf")
                overrides = {**(tent_env if implementation == "tent" else {}), "ORBITE_BENCH_TAIL_TRACE": "1"}
                command = ["sudo", perf, "record", "-q", "-a", "--clockid", "mono", "-m", "256", "-o", data]
                for event in events:
                    command += ["-e", event]
                command += ["--", "env", *[f"{k}={v}" for k, v in overrides.items()], exe,
                            "io_uring", op, 4096, batch, layout, args.warmup, samples, args.workers, path]
                try:
                    run(name, command)
                    result = json.loads((root / (name + ".stdout")).read_text())
                    if result.get("verified") is not True or len(result["samples_ns"]) != samples:
                        raise ValueError("tail profile did not complete all byte checks")
                    windows = []
                    for line in (root / (name + ".stderr")).read_text().splitlines():
                        if line.startswith("ORBITE_SAMPLE "):
                            i, begin, end, pid = map(int, line.split()[1:])
                            if i != len(windows) or end <= begin:
                                raise ValueError("invalid sample timeline")
                            windows.append(dict(sample=i, begin_ns=begin, end_ns=end, pid=pid,
                                                measured=i >= args.warmup))
                    if len(windows) != samples + args.warmup:
                        raise ValueError("missing sample timelines")
                    (root / (name + ".windows.json")).write_text(json.dumps(windows) + "\n")
                    if data.stat().st_size > 512 << 20:
                        raise ValueError("unexpectedly large diagnostic recording")
                    run(name + "-events", ["sudo", perf, "script", "--ns", "-i", data,
                                             "-F", "comm,pid,tid,cpu,time,event,trace"])
                    if "lost" in (root / (name + "-events.stderr")).read_text().lower():
                        raise ValueError("lost trace events; timeline is incomplete")
                    records.append(dict(name=name, samples=samples, warmup=args.warmup,
                                        verified=True, timings="diagnostic only"))
                    (root / "summary.json").write_text(json.dumps(records, indent=2) + "\n")
                    print(json.dumps(records[-1]), flush=True)
                finally:
                    subprocess.run(["sudo", "rm", "-f", path], check=True)
    return len(records)
