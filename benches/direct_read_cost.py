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
        ("raw",size,1), ("raw",8<<20,2), ("raw-thp",size,1),
        ("xchg-keep",8<<20,2), ("xchg-keep-thp",4<<20,2),
        ("xchg-keep-thp",8<<20,2), ("xchg-keep-thp",16<<20,1),
        ("xchg-keep-thp",size,1), ("xchg-keep-thp-aligned",8<<20,2),
    ]
    for trial in range(4):
        order = variants[trial:] + variants[:trial]
        if trial % 2: order.reverse()
        for mode,chunk,slots in order:
            p = subprocess.run([exe,mode,str(Path(directory)/"payload"),str(size),str(chunk),str(slots),"15"],
                               text=True, capture_output=True)
            if p.returncode:
                print("unavailable",mode,chunk,slots,p.stderr,flush=True)
                if mode.startswith(("fixed","move","snap")) and any(e in p.stderr for e in
                        ("Operation not permitted","Cannot allocate memory","Permission denied",
                         "Invalid argument","Resource temporarily unavailable","Function not implemented",
                         "Inappropriate ioctl for device","Device or resource busy")):
                    print(json.dumps({"mode":mode,"chunk":chunk,"size":size,"slots":slots,
                                      "process_trial":trial,"unavailable":p.stderr.strip()}),file=raw,flush=True)
                    continue
                raise RuntimeError((p.args,p.stderr[-1000:]))
            row=json.loads(p.stdout); assert row["verified"] and len(row["trials"])==15
            if row["read_bytes"] < size*(3+15):
                raise RuntimeError(("insufficient block-read evidence",mode,chunk,slots,row["read_bytes"]))
            row["process_trial"]=trial
            print(json.dumps(row),file=raw,flush=True)
            series.setdefault((size,mode,chunk,slots),[]).append(row)
for key, rows in series.items():
    stages = {s:[round(median(sum(x[s][c] for x in proc["trials"])/len(proc["trials"]) for proc in rows)
                           /(1e6 if c<2 else 1),3) for c in range(3)]
              for s in ("setup","read","copy","retire","total")}
    print(key,stages,flush=True)
