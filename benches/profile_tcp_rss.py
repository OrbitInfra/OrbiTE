"""Untraced process RSS and separate allocation profiles for a loopback TCP READ."""
import json
from statistics import median

from run import RSS_PREFIX, peak_rss, summarize


def measure(out, rust, tent, run, tent_env):
    size = 64 << 20
    executables = [("orbite", rust), ("tent", tent)]
    peaks = {name: [] for name, _ in executables}
    for trial in range(3):
        for name, exe in (reversed(executables) if trial % 2 else executables):
            label = f"{name}-rss-{trial}"
            text = run(label, ["/usr/bin/time", "-f", "\\n" + RSS_PREFIX + "%M", "--",
                               exe, "tcp", "read", size, 1, "contiguous", 3, 100, 2, "-"],
                       overrides=tent_env if name == "tent" else None)
            row = json.loads(text)
            summarize(row, 100, size)
            if name == "orbite" and row["physical_owners"] != 1:
                raise ValueError("expected one TCP READ owner")
            peaks[name].append(peak_rss((out / (label + ".stderr")).read_text()))
    report = {"peak_rss_kib": peaks, "median_rss_ratio": median(peaks["orbite"]) / median(peaks["tent"])}
    (out / "tcp-read-rss.json").write_text(json.dumps(report, indent=2) + "\n")
    for name, exe in executables:
        dest = out / (name + "-heap")
        dest.mkdir()
        text = run(name + "-heaptrack", ["heaptrack", "-o", dest / "trace", exe,
                   "tcp", "read", size, 1, "contiguous", 1, 3, 2, "-"], overrides=tent_env if name == "tent" else None)
        row, = [json.loads(line) for line in text.splitlines() if line.startswith("{")]
        summarize(row, 3, size)
        trace, = dest.glob("trace*")
        run(name + "-heaptrack-print", ["heaptrack_print", "-f", trace, "-F", dest / "peak.stacks",
            "--flamegraph-cost-type", "peak", "-M", dest / "timeline.massif", "--massif-detailed-freq", "1"])
