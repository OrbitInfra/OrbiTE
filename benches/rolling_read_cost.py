"""Rotate private window configurations on identical ext4 and kernel setup."""
from pathlib import Path
from statistics import median, mean
import json
import subprocess
import sys

exe, mount, output = sys.argv[1:]
out = Path(output)
out.mkdir()
size = 64 << 20
samples = 30
variants = [
    ("raw", size, 1), ("huge", size, 1),
    ("huge-keep", 4<<20, 2), ("huge-roll-keep", 4<<20, 2),
    ("huge-roll-keep-nt", 4<<20, 2), ("huge-roll-keep-nt", 2<<20, 4),
    ("fixed-huge-roll-keep-nt", 2<<20, 4), ("huge-roll-nt", 2<<20, 4),
    ("huge-roll-keep-nt", 8<<20, 2), ("fixed-huge-roll-keep-nt", 4<<20, 4),
]
boot = Path("/proc/sys/kernel/random/boot_id").read_text().strip()
for i in range(6):
    shift = (i // 2 * 3) % len(variants)
    ordered = variants[shift:] + variants[:shift]
    if i % 2:
        ordered = list(reversed(ordered))
    for mode, chunk, slots in ordered:
        cmd = [exe, mode, str(Path(mount)/"payload"), str(size), str(chunk), str(slots), str(samples)]
        data = json.loads(subprocess.check_output(cmd, text=True))
        ts = data["trials"]
        assert data["verified"] and data["read_bytes"] >= size*(samples + 3)
        assert len(ts)==samples and data["size"]==size and data["chunk"]==chunk and data["slots"]==slots
        data.update(boot=boot, process=i, order=ordered)
        (out/f"{i}-{mode}-{chunk}-{slots}.json").write_text(json.dumps(data))
        rss = int(data["hwm"].split()[1])
        ns=[t["total"][0] for t in ts]
        row=[size*1e9/mean(ns)/2**30,mean(t["total"][1] for t in ts)/size,max(ns)/1e6,rss,
             mean(t["arrived_during_copy"] for t in ts)]
        print(i,mode,chunk,slots,*[round(v,6) for v in row],flush=True)
