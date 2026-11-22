"""Untraced process RSS and separate allocation profiles for a loopback TCP READ."""
import json
from statistics import median

from run import RSS_PREFIX, peak_rss, validate_samples


def measure(out, rust, tent, run, tent_env):
    size = 64 << 20
    executables = [("orbite", rust), ("tent", tent)]
    peaks = {name: [] for name, _ in executables}

    def verify(name, row, count):
        validate_samples(row, count, size)
        if name == "orbite" and (type(row["physical_owners"]) is not int or row["physical_owners"] != 1):
            raise ValueError("expected one TCP READ owner")

    for trial in range(3):
        for name, exe in (reversed(executables) if trial % 2 else executables):
            label = f"{name}-rss-{trial}"
            text = run(label, ["/usr/bin/time", "-f", "\\n" + RSS_PREFIX + "%M", "--",
                               exe, "tcp", "read", size, 1, "contiguous", 3, 100, 2, "-"],
                       overrides=tent_env if name == "tent" else None)
            row = json.loads(text)
            verify(name, row, 100)
            peaks[name].append(peak_rss((out / (label + ".stderr")).read_text()))
    report = {"peak_rss_kib": peaks, "median_rss_ratio": median(peaks["orbite"]) / median(peaks["tent"])}
    (out / "tcp-read-rss.json").write_text(json.dumps(report, indent=2) + "\n")
    for name, exe in executables:
        dest = out / (name + "-heap")
        dest.mkdir()
        text = run(name + "-heaptrack", ["heaptrack", "-o", dest / "trace", exe,
                   "tcp", "read", size, 1, "contiguous", 1, 3, 2, "-"], overrides=tent_env if name == "tent" else None)
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
