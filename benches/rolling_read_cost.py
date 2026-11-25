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
samples = 24
variants = [
    ("raw-A", size, 1), ("raw-B", size, 1), ("huge", size, 1),
    ("fixed-huge-roll-keep-nt", 2<<20, 4),
    ("fixed-huge-roll-keep-nt",512<<10,8),
    ("fixed-huge-roll-keep-nt",256<<10,16),
    ("fixed-huge-roll-keep-nt",128<<10,16),
    ("fixed-huge-roll-keep-off512-nt",512<<10,8),
    ("fixed-huge-roll-keep-off2k-nt",512<<10,8),
    ("fixed-huge-roll-keep-gnt",512<<10,8),
    ("fixed-huge-roll-keep-off512-gnt",512<<10,8),
    ("fixed-huge-roll-keep-off2k-gnt",256<<10,16),
    ("fixed-huge-roll-keep-off2k-burst-gnt",256<<10,16),
    ("fixed-huge-roll-keep-off512-burst-gnt",128<<10,16),
    ("fixed-huge-roll-keep-off512-burst-gnt",512<<10,8),
    ("huge-roll-keep-off512-burst-gnt",512<<10,8),
    ("fixed-huge-roll-keep-off512-burst",256<<10,16),
    ("fixed-huge-roll-keep-off2k-gnt",2<<20,4),
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
        if data.get("unsupported_bias"):
            print("unsupported address offset",mode,chunk,slots,data,flush=True)
            (out/f"{i}-{mode}-{chunk}-{slots}-ineligible.json").write_text(json.dumps(data))
            continue
        ts = data["trials"]
        assert data["verified"] and data["read_bytes"] >= size*(samples + 4) and len(data["probe"])==1
        assert len(ts)==samples and data["size"]==size and data["chunk"]==chunk and data["slots"]==slots
        data.update(boot=boot, process=i, order=ordered)
        (out/f"{i}-{mode}-{chunk}-{slots}.json").write_text(json.dumps(data))
        rss = int(data["hwm"].split()[1])
        ns=[t["total"][0] for t in ts]
        row=[size*1e9/mean(ns)/2**30,mean(t["total"][1] for t in ts)/size,max(ns)/1e6,rss,
             mean(t["arrived_during_copy"] for t in ts)]
        print(i,mode,chunk,slots,*[round(v,6) for v in row],flush=True)
