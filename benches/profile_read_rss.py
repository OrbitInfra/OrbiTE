"""Untraced process RSS and separate heap profiles for a 64 MiB READ."""
import json
from pathlib import Path
from statistics import median
from tempfile import TemporaryDirectory

from run import RSS_PREFIX, peak_rss, validate_samples


def measure(out, rust, tent, run, tent_env, *, tmpfs=None):
    size = 64 << 20
    profile = "tcp" if tmpfs is None else "tmpfs"
    backend = "tcp" if tmpfs is None else "io_uring"
    executables = [("orbite", rust), ("tent", tent)]
    peaks = {name: [] for name, _ in executables}

    def verify(name, row, count):
        validate_samples(row, count, size)
        if name == "orbite" and (type(row["physical_owners"]) is not int or row["physical_owners"] != 1):
            raise ValueError(f"expected one {profile} READ owner")
        if name == "orbite" and tmpfs is not None and row["direct_io"] is not False:
            raise ValueError("expected buffered io_uring on tmpfs")

    def child(label, name, exe, prefix, warmup, count):
        args = [*prefix, exe, backend, "read", size, 1, "contiguous", warmup, count, 2]
        env = tent_env if name == "tent" else None
        if tmpfs is None:
            return run(label, [*args, "-"], overrides=env)
        with TemporaryDirectory(prefix="orbite-read-rss-", dir=tmpfs) as scratch:
            return run(label, [*args, Path(scratch) / "payload"], overrides=env)

    for trial in range(3):
        for name, exe in (reversed(executables) if trial % 2 else executables):
            label = f"{name}-rss-{trial}"
            text = child(label, name, exe, ["/usr/bin/time", "-f", "\\n" + RSS_PREFIX + "%M", "--"], 3, 100)
            row = json.loads(text)
            verify(name, row, 100)
            peaks[name].append(peak_rss((out / (label + ".stderr")).read_text()))
    report = {"peak_rss_kib": peaks, "median_rss_ratio": median(peaks["orbite"]) / median(peaks["tent"])}
    (out / (profile + "-read-rss.json")).write_text(json.dumps(report, indent=2) + "\n")
    for name, exe in executables:
        dest = out / (name + "-heap")
        dest.mkdir()
        text = child(name + "-heaptrack", name, exe, ["heaptrack", "-o", dest / "trace"], 1, 3)
        row, = [json.loads(line) for line in text.splitlines() if line.startswith("{")]
        verify(name, row, 3)
        trace, = dest.glob("trace*")
        run(name + "-heaptrack-print", ["heaptrack_print", "-f", trace, "-F", dest / "peak.stacks",
            "--flamegraph-cost-type", "peak", "-M", dest / "timeline.massif", "--massif-detailed-freq", "1"])
        stacks = (dest / "peak.stacks").read_text().splitlines()
        if not any(int(line.rsplit(" ", 1)[1]) > 0 for line in stacks):
            raise ValueError(f"{name}: missing live allocation stacks")
        with (dest / "timeline.massif").open() as timeline:
            heap_samples = (line.removeprefix("mem_heap_B=") for line in timeline if line.startswith("mem_heap_B="))
            if not any(int(sample) > 0 for sample in heap_samples):
                raise ValueError(f"{name}: missing positive heap sample in timeline")
