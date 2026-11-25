"""Matched direct READ, ordinary staging, and private kernel pipe windows."""
import json
import random
import statistics
import subprocess
import sys
from pathlib import Path

exe, directory, result = sys.argv[1:]
out = Path(result)
out.mkdir()
size = 64 << 20
variants = [
    ("raw",size,1), ("huge",size,1), ("pipegroup",1<<20,8),
    ("pipegroup",1<<20,32), ("pipegroup-keep",1<<20,8),
    ("pipegroup-keep",256<<10,16),
]
generator = random.Random(761)
generator.shuffle(variants)
for trial in range(6):
    order = variants[trial:]+variants[:trial]
    for mode,chunk,slots in order:
        cmd=[exe,mode,str(Path(directory)/"payload"),str(size),str(chunk),str(slots),"15"]
        process=subprocess.run(cmd, capture_output=True, text=True, check=False)
        if process.returncode:
            print("failed",cmd,process.stderr,flush=True)
            raise RuntimeError(process.returncode)
        row=json.loads(process.stdout)
        assert row["verified"] and len(row["trials"])==15
        assert row["read_bytes"]>=size*18, (mode, row["read_bytes"])
        (out/f"{trial}-{mode}-{chunk}-{slots}.json").write_text(process.stdout)
        wall=[m["total"][0] for m in row["trials"]]
        cpu=[m["total"][1] for m in row["trials"]]
        print(trial,mode,chunk,slots,"GiB/s CPU_ns/B max_us",size*1e9/statistics.mean(wall)/2**30,
              statistics.mean(cpu)/size,max(wall)/1000,flush=True)
