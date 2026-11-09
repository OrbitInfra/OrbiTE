"""Bounded, diagnostic-only sample windows and kernel completion/scheduler traces."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile


def record(out, executables, args, env, tent_env):
    perf = sorted(Path("/usr/lib/linux-tools").glob("*/perf"))
    perf = str(perf[-1]) if perf else shutil.which("perf")
    if not perf:
        raise RuntimeError("tail profiling requires perf and root tracepoint access")
    root = out / "tail"
    root.mkdir()
    commands, records = [], []

    def run(name, command, destination=root):
        command = list(map(str, command))
        commands.append({"name": name, "command": command})
        (root / "commands.json").write_text(json.dumps(commands, indent=2) + "\n")
        with (destination / (name + ".stdout")).open("w") as stdout, (destination / (name + ".stderr")).open("w") as stderr:
            subprocess.run(command, env=env, stdout=stdout, stderr=stderr, check=True, timeout=600)

    # Keep the kernel's field definitions with the evidence: names/layouts vary
    # across kernels. All-CPU recording includes io-wq and interrupt completions.
    run("formats", ["sudo", "sh", "-c", "cat " + " ".join(
        f"/sys/kernel/tracing/events/{group}/*/format" for group in ("io_uring", "block", "iomap", "filemap"))])
    run("devices", ["lsblk", "-O", "-J"])
    run("queues", ["sh", "-c", "for q in /sys/block/*/queue; do echo \"$q\"; "
                  "for k in scheduler nr_requests max_sectors_kb rotational wbt_lat_usec; "
                  "do test ! -r \"$q/$k\" || { echo \"$k\"; cat \"$q/$k\"; }; done; done"])
    events = ["io_uring:*", "sched:sched_switch", "sched:sched_wakeup",
              "sched:sched_process_fork", "sched:sched_process_exit",
              "syscalls:sys_enter_io_uring_setup", "syscalls:sys_exit_io_uring_setup",
              "syscalls:sys_enter_io_uring_enter", "syscalls:sys_exit_io_uring_enter",
              "syscalls:sys_enter_clock_nanosleep", "syscalls:sys_exit_clock_nanosleep",
              "syscalls:sys_enter_close", "syscalls:sys_exit_close",
              "syscalls:sys_enter_munmap", "syscalls:sys_exit_munmap",
              "syscalls:sys_enter_futex", "syscalls:sys_exit_futex",
              "syscalls:sys_enter_fdatasync", "syscalls:sys_exit_fdatasync",
              "block:block_rq_issue", "block:block_rq_complete", "block:block_rq_insert",
              "block:block_bio_queue", "block:block_bio_backmerge", "block:block_bio_frontmerge",
              "block:block_split", "block:block_plug", "block:block_unplug",
              "iomap:iomap_dio_rw_begin", "iomap:iomap_dio_complete",
              "iomap:iomap_iter", "iomap:iomap_iter_dstmap",
              "filemap:mm_filemap_delete_from_page_cache"]
    shapes = [("ext4", 64, "gapped"), ("ext4", 64, "contiguous"), ("ext4", 8, "gapped"),
              ("ext4", 8, "contiguous"), ("ext4", 1, "contiguous"),
              ("tmpfs", 1, "contiguous")]
    shapes = [(*s, 0) for s in shapes] + [("ext4", 64, layout, 10000)
                                         for layout in ("gapped", "contiguous")]
    shapes = [(*s, False) for s in shapes] + [("ext4", 64, layout, 0, True)
                                             for layout in ("gapped", "contiguous")]
    samples = max(args.samples, 1000)
    for filesystem, batch, layout, settle_us, legacy_verify in shapes:
        for op in ("read", "write"):
            for implementation, exe in executables:
                if legacy_verify and implementation != "tent":
                    continue  # Diagnostic reconstruction of the earlier C++ harness.
                name = f"{implementation}-{filesystem}-{op}-{batch}-{layout}"
                if settle_us:
                    name += "-settled"
                if legacy_verify:
                    name += "-legacy"
                directory = args.ext4 if filesystem == "ext4" else args.tmpfs
                path = directory.resolve() / ("orbite-tail-" + name)
                if path.exists():
                    raise FileExistsError(path)
                # The collector must not compete with the measured file for
                # ext4 I/O. Stage trace and sample-window output on tmpfs, then
                # copy them only after the benchmark has exited.
                if shutil.disk_usage(args.tmpfs).free < 512 << 20:
                    raise RuntimeError("tail recording needs 512 MiB of free tmpfs")
                scratch = tempfile.TemporaryDirectory(prefix="orbite-tail-", dir=args.tmpfs)
                temporary = Path(scratch.name)
                data = temporary / (name + ".perf")
                overrides = {**(tent_env if implementation == "tent" else {}), "ORBITE_BENCH_TAIL_TRACE": "1",
                             "ORBITE_BENCH_RESET_SETTLE_US": str(settle_us)}
                if legacy_verify:
                    overrides["ORBITE_BENCH_LEGACY_VERIFY"] = "1"
                command = ["sudo", perf, "record", "-q", "-a", "--clockid", "mono", "-m", "256", "-o", data]
                for event in events:
                    command += ["-e", event]
                command += ["--", "env", *[f"{k}={v}" for k, v in overrides.items()], exe,
                            "io_uring", op, 4096, batch, layout, args.warmup, samples, args.workers, path]
                try:
                    os.sync()  # Finish earlier evidence writes outside sample windows.
                    run(name, command, temporary)
                    for suffix in (".stdout", ".stderr"):
                        shutil.copyfile(temporary / (name + suffix), root / (name + suffix))
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
                    subprocess.run(["sudo", "chmod", "a+r", data], check=True)
                    run(name + "-events", ["sudo", perf, "script", "--ns", "-i", data,
                                             "-F", "comm,pid,tid,cpu,time,event,trace"])
                    shutil.copyfile(data, root / (name + ".perf"))
                    if "lost" in (root / (name + "-events.stderr")).read_text().lower():
                        raise ValueError("lost trace events; timeline is incomplete")
                    records.append(dict(name=name, samples=samples, warmup=args.warmup, settle_us=settle_us, legacy_verify=legacy_verify,
                                        verified=True, timings="diagnostic only"))
                    (root / "summary.json").write_text(json.dumps(records, indent=2) + "\n")
                    print(json.dumps(records[-1]), flush=True)
                finally:
                    subprocess.run(["sudo", "rm", "-f", path], check=True)
                    scratch.cleanup()
                # Separate, short syscall run: confirm the actual descriptor flags,
                # not just the source spelling. Never use ptrace timings as rows.
                run(name + "-fds", ["strace", "-f", "-yy", "-ttt", "-o", root / (name + ".strace"),
                                     "-e", "trace=openat,fcntl,io_uring_setup,io_uring_enter,fdatasync",
                                     "env", *[f"{k}={v}" for k, v in overrides.items()], exe,
                                     "io_uring", op, 4096, batch, layout, args.warmup, 3, args.workers, path])
                path.unlink()
                if json.loads((root / (name + "-fds.stdout")).read_text()).get("verified") is not True:
                    raise ValueError("descriptor trace failed byte checks")
    from analyze_io_tail import analyze, controls
    controls()
    analyze(root)
    return len(records)
