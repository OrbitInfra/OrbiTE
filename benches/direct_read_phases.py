"""Balanced same-boot comparison of demand-faulted and explicitly resident 64 MiB READs."""
from pathlib import Path
from statistics import median
from hashlib import sha256
import json
import os
import platform
import subprocess
import sys

dir_, result = map(Path, sys.argv[1:3])
out = result
out.mkdir()
modes = ["raw", "cold", "cold-touch", "hot", "hot-touch", "base", "base-touch"]
tools = ["stable", "1.85.0"]
samples = 18
meta = {
    "boot": Path("/proc/sys/kernel/random/boot_id").read_text().strip(), "kernel": platform.release(),
    "ext4": subprocess.check_output(["findmnt","-n","-o","FSTYPE,TARGET,SOURCE","-T",str(dir_)],text=True).strip(),
    "thp": Path("/sys/kernel/mm/transparent_hugepage/enabled").read_text().strip(),
    "head": subprocess.check_output(["git","rev-parse","HEAD"],text=True).strip(),
    "toolchains": {tc:subprocess.check_output(["rustc","+"+tc,"-Vv"],text=True).strip() for tc in tools},
    "executables": {tc:sha256((Path(os.environ["RUNNER_TEMP"])/tc/"release/examples/direct_read_phases").read_bytes()).hexdigest() for tc in tools},
    "timing": "wall ns, process CPU ns, minor faults per sample. Timed from pre-allocation to post-free. No mincore except first discarded warmup.",
    "shape": [64 << 20, 1], "warmup":3, "measured":samples,
}
assert meta["ext4"].split()[0]=="ext4"
(out/"metadata.json").write_text(json.dumps(meta,indent=2))
print(meta,flush=True)
stats = {}
for trial in range(len(modes)):
    order=modes[trial:]+modes[:trial]
    for position,mode in enumerate(order):
        for tc in tools[::1 if (trial+position)%2==0 else -1]:
            exe=Path(os.environ["RUNNER_TEMP"])/tc/"release/examples/direct_read_phases"
            row=json.loads(subprocess.check_output([str(exe), mode, str(dir_/"phase-file"), str(samples)],text=True))
            assert row["verified"] and row["mode"]==mode and row["bytes"]==64<<20
            assert len(row["samples"])==samples
            assert row["resident_pages"][0][2]==row["resident_pages"][1]
            assert row["read_bytes"] >= (64<<20)*(samples+3)
            row.update(trial=trial,position=position,rust=tc,boot=meta["boot"])
            (out/f"{tc}-{mode}-{trial}.json").write_text(json.dumps(row))
            stats.setdefault((tc,mode),[]).append({stage:[median(val) for val in zip(*[x[stage] for x in row["samples"]])] for stage in row["samples"][0]})
    print(f"all seven variants on both compilers passed round {trial+1}/7",flush=True)
for (tool,mode),vectors in stats.items():
    print(tool,mode, json.dumps({k:[round(median(arr[:])/scale,4) for arr,scale in zip(zip(*[v[k] for v in vectors]),(1e6,1e6,1))] for k in vectors[0]}),flush=True)

