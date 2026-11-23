"""Run fresh direct-read processes on the same ext4, rotating mechanism order."""
import json
import os
from pathlib import Path
from statistics import median
import subprocess
import sys

exe, directory, results = sys.argv[1:]
out = Path(results)
out.mkdir()
raw = (out / "samples.jsonl").open("w")
series = {}
for size in (1<<20, 64<<20):
    variants = [
        ("raw",size,1), ("private",size,1), ("huge",size,1),
        ("keep",size,1), ("window",1<<20,4), ("fixed",1<<20,4),
        ("fixed-keep",1<<20,4), ("fixed-keep",256<<10,8),
        ("fixed-keep",2<<20,2), ("window",4<<20,2), ("pipe",1<<20,1),
    ]
    for trial in range(4):
        order = variants[trial:] + variants[:trial]
        if trial % 2: order.reverse()
        for mode,chunk,slots in order:
            p = subprocess.run([exe,mode,str(Path(directory)/"payload"),str(size),str(chunk),str(slots),"15"],
                               text=True, capture_output=True)
            if p.returncode:
                print("unavailable",mode,chunk,slots,p.stderr,flush=True)
                if mode.startswith("fixed") and any(e in p.stderr for e in
                        ("Operation not permitted","Cannot allocate memory","Permission denied")):
                    print(json.dumps({"mode":mode,"chunk":chunk,"size":size,"slots":slots,
                                      "process_trial":trial,"unavailable":p.stderr.strip()}),file=raw,flush=True)
                    continue
                raise RuntimeError((p.args,p.stderr[-1000:]))
            row=json.loads(p.stdout); assert row["verified"] and len(row["trials"])==15
            if row["read_bytes"] < size*(3+15):
                print("insufficient block-read evidence",mode,chunk,slots,row["read_bytes"],flush=True)
            row["process_trial"]=trial
            print(json.dumps(row),file=raw,flush=True)
            series.setdefault((size,mode,chunk,slots),[]).append(row)
for key, rows in series.items():
    stages = {s:[round(median(sum(x[s][c] for x in proc["trials"])/len(proc["trials"]) for proc in rows)
                           /(1e6 if c<2 else 1),3) for c in range(3)]
              for s in ("setup","read","copy","retire","total")}
    print(key,stages,flush=True)
