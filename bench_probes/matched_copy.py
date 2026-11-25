"""Matched uninstrumented 64 MiB single READs vs the fixed reference and PR base."""
from hashlib import sha256
from pathlib import Path
from statistics import median
import json,os,subprocess,sys
ext,out=map(Path,sys.argv[1:3])
out.mkdir()
tmp=Path(os.environ["RUNNER_TEMP"])
rust=["stable","1.85.0"]
groups=["fixed","head","candidate"]
all_groups=groups+["candidate-slot"]
size=64<<20
boot=Path("/proc/sys/kernel/random/boot_id").read_text().strip()
meta={"boot":boot,"kernel":os.uname().release,
      "mount":subprocess.check_output(["findmnt","-n","-o","FSTYPE,SOURCE,TARGET","-T",str(ext)],text=True).strip(),
      "hardware":subprocess.check_output(["lscpu"],text=True),"toolchains":{},"groups":{},
      "caller_sha256":sha256((tmp/"head"/"benches/transfer.rs").read_bytes()).hexdigest(),
      "optional_slot_caller_sha256":sha256(Path("benches/transfer.rs").read_bytes()).hexdigest(),
      "runner":subprocess.check_output(["git","rev-parse","HEAD"],text=True).strip()}
assert meta["mount"].split()[0]=="ext4" and " avx2 " in meta["hardware"]
exes={}
for tc in rust:
    meta["toolchains"][tc]=subprocess.check_output(["rustc","+"+tc,"-Vv"],text=True)
    for g in all_groups:
        exe=Path((tmp/f"{tc}-{g}.exe").read_text().strip()); exes[tc,g]=exe
        rev=subprocess.check_output(["git","rev-parse","HEAD"],cwd=tmp/("candidate" if g=="candidate-slot" else g),text=True).strip()
        meta["groups"].setdefault(g,{"sha":rev,"binary":{}})["binary"][tc]=sha256(exe.read_bytes()).hexdigest()
(out/"meta.json").write_text(json.dumps(meta,indent=2))
print(meta["groups"],"boot",boot,flush=True)
trials={}
for i in range(3):
    order=groups[i:]+groups[:i]+["candidate-slot"]
    for pos,g in enumerate(order):
        for tc in rust[::1 if (i+pos)%2==0 else -1]:
            env=os.environ.copy()
            if g=="candidate-slot":env["ORBITE_LARGE_PHASE_POOL"]="1"
            assert "ORBITE_LARGE_PHASE" not in env
            path=ext/f"{tc}-{g}-{i}"
            stats=out/f"{tc}-{g}-{i}.time"
            command=[str(exes[tc,g]),"io_uring","read",str(size),"1","contiguous","3","100","2",str(path)]
            r=subprocess.run(["/usr/bin/time","-f","%M %I","-o",str(stats),*command],env=env,capture_output=True,text=True)
            if r.returncode: print(r.stderr,flush=True);r.check_returncode()
            row=json.loads(r.stdout);rss,inblock=map(int,stats.read_text().split())
            assert row["verified"] and row["direct_io"] and row["physical_owners"]==1 and row["payload_bytes"]==size
            assert len(row["cpu_ns"])==len(row["samples_ns"])==100 and all(t>0 for t in row["cpu_ns"]+row["samples_ns"])
            assert "owner-phases" not in r.stderr
            row.update(tc=tc,group=g,i=i,position=pos,boot=boot,peak_rss_kib=rss,inblock=inblock,command=command)
            (out/f"{tc}-{g}-{i}.json").write_text(json.dumps(row))
            path.unlink(missing_ok=True)
            trials.setdefault((tc,g),[]).append([100*size*1e9/(sum(row["samples_ns"])*2**30),
                   sum(row["cpu_ns"])/(100*size),sorted(row["samples_ns"])[98]/1e6,rss/1024])
    print("public untraced round",i+1,"complete",flush=True)
for tc in rust:
    vals={g:list(map(median,zip(*trials[tc,g]))) for g in all_groups}
    for g in all_groups:
        ratios={ref:[b/a for a,b in zip(vals[ref],vals[g])] for ref in ["fixed","head"]}
        print(tc,g,"GiB/s ns/byte p99-ms RSS-MiB",*[round(x,5) for x in vals[g]],
              "ratios",json.dumps(ratios), "passes_fixed",
              [x>=.95 if j==0 else x<=(1.10,1.10,1.05,1.10)[j] for j,x in enumerate(ratios["fixed"])],flush=True)

