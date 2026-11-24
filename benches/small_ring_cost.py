"""Six matched A/A/B orders on the same direct ext4 file and boot."""
from itertools import permutations
from pathlib import Path
from statistics import mean, median
import json, os, subprocess, sys

exe, directory, results = sys.argv[1:]
out = Path(results)
out.mkdir()
size, block, depth, samples = 32768, 4096, 8, 1000
labels = {"fresh-A": "protected", "fresh-B":"protected", "reused":"protected-ring"}
summaries = {name:[] for name in labels}
for n,order in enumerate(permutations(labels)):
    for label in order:
        command = [exe, labels[label], str(Path(directory) / "payload"),str(size),str(block),str(depth),str(samples)]
        row=json.loads(subprocess.check_output(command,text=True))
        assert row["verified"] and row["read_bytes"]>=size*(samples+3)
        assert (row["size"],row["chunk"],row["slots"])==(size,block,depth) and len(row["trials"])==samples
        row.update(label=label, order=order, trial=n, boot=Path("/proc/sys/kernel/random/boot_id").read_text().strip())
        (out/f"{n}-{label}.json").write_text(json.dumps(row))
        ts=row["trials"]
        summaries[label].append([
            size*1e9/mean(t["total"][0] for t in ts)/2**30,
            mean(t["total"][1] for t in ts)/size,
            sorted(t["total"][0] for t in ts)[989]/1000,
            median(t["setup"][0] for t in ts)/1000,
            median(t["retire"][0] for t in ts)/1000,
            median(t["read"][0] for t in ts)/1000])
for label, vals in summaries.items():
    print(label,"GiB/s CPU-ns/B p99-us setup-us retire-us native-us",*[round(median(xs),4) for xs in zip(*vals)])

