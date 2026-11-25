"""Compare one 64 MiB io_uring CQE and identical confirmed-prefix publication."""
import hashlib, json, math, os, subprocess, sys
from pathlib import Path
from statistics import mean, median
mount, out=map(Path,sys.argv[1:3])
out.mkdir()
size=64<<20
reps=3
samples=100
modes=["raw-keep","huge","huge-one","huge-keep-one","fixed-huge-readv-keep-one",
       "fixed-huge-keep-one","huge-keep-two","fixed-huge-keep-two"]
tcs=["stable","1.85.0"]
def cmd(*a): return subprocess.check_output(a,text=True).strip()
meta=dict(boot=Path("/proc/sys/kernel/random/boot_id").read_text().strip(),uname=cmd("uname","-a"),mount=cmd("findmnt","-n","-o","SOURCE,FSTYPE,OPTIONS","-T",str(mount)),cpus=cmd("nproc"),cpu=cmd("lscpu"),limits=Path("/proc/self/limits").read_text(),shas={},toolchains={},warmups=3,samples=samples,rounds=reps,size=size)
assert cmd("findmnt","-n","-o","FSTYPE","-T",str(mount))=="ext4"
for tc in tcs:
    exe=Path(os.environ["RUNNER_TEMP"])/tc/"release/examples/direct_read_cost"
    meta["shas"][tc]=hashlib.sha256(exe.read_bytes()).hexdigest()
    meta["toolchains"][tc]=cmd("rustc","+"+tc,"-Vv")
(out/'meta.json').write_text(json.dumps(meta))
results={tc:{m:[] for m in modes} for tc in tcs}
for i in range(reps):
    k=(i*3)%len(modes)
    order=modes[k:]+modes[:k]
    if i%2:order=order[::-1]
    for pos,mode in enumerate(order):
        for tc in (tcs if (pos+i)%2==0 else tcs[::-1]):
            exe=Path(os.environ["RUNNER_TEMP"])/tc/"release/examples/direct_read_cost"
            row=json.loads(cmd(str(exe),mode,str(mount/'singleton-fixed-control'),str(size),str(size),"1",str(samples)))
            assert row["verified"] and row["read_bytes"]>=size*(samples+4) and len(row["trials"])==samples
            assert row["fixed_bytes"]==(size if mode.startswith("fixed") else 0)
            assert row["retained_scratch_bytes"]==(size if "keep" in mode and not mode.startswith("raw") else 0)
            row.update(tc=tc,round=i,pos=pos,boot=meta["boot"])
            ts=row["trials"]; delta=[t["total"][0] for t in ts]
            values=[size*1e9/mean(delta)/2**30,mean(t["total"][1] for t in ts)/size,sorted(delta)[98]/1e6,int(row["hwm"].split()[1])/1024,
                    mean(t["read"][0] for t in ts)/1e6,mean(t["read"][1] for t in ts)/1e6,mean(t["copy"][0] for t in ts)/1e6,mean(t["copy"][1] for t in ts)/1e6,row["cold_setup"][0]/1e6,row["fixed_bytes"]/2**20]
            results[tc][mode].append(values)
            (out/f'{tc}-{mode}-{i}.json').write_text(json.dumps(row))
            print("TRIAL",i,pos,tc,mode,*(round(v,4) for v in values),flush=True)
print("COLUMNS GiB/s cpu_ns/B p99ms rssMiB read_ms read_cpu_ms copy_ms copy_cpu_ms keep_setup_ms pinned_MiB")
for tc in tcs:
    med={m:list(map(median,zip(*results[tc][m]))) for m in modes}
    for mode in modes:
        print("MEDIAN",tc,mode,*(round(v,4) for v in med[mode]),"vsraw",*[round(v/b,3) for v,b in zip(med[mode][:4],med["raw-keep"][:4])],
              "vsprotected",*[round(v/b,3) for v,b in zip(med[mode][:4],med["huge"][:4])],flush=True)
