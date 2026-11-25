"""One request in flight through the ordinary public benchmark and production block owner."""
from pathlib import Path
from statistics import median
from hashlib import sha256
import os,json,re,sys,subprocess

dir_,out = map(Path,sys.argv[1:3])
out.mkdir()
labels=["parent","disabled","natural","prefault","pooled","pooled-touch"]
tools=["stable","1.85.0"]
stages=["acquire","touch","prepared_to_cqe","copy","release"]
boot=Path("/proc/sys/kernel/random/boot_id").read_text().strip()
outroot=Path(os.environ["RUNNER_TEMP"])
exe={t:{k:Path((outroot/f"{t}-{k}.exe").read_text().strip()) for k in ["plain","candidate","fixed"]} for t in tools}
meta={"cpu":subprocess.check_output(["lscpu"],text=True),"bench_sha256":sha256(Path("benches/transfer.rs").read_bytes()).hexdigest(),"fixed":"12d1e83964a596d9b578e9d2879b5cd78e776da6","boot":boot,"kernel":os.uname().release,"trials":3,"samples":100,"warmup":3,"base":"ea265fdecbea48e4f05dff32314f966251943f52",
      "head":subprocess.check_output(["git","rev-parse","HEAD"],text=True).strip(),
      "binary_sha256":{t:{k:sha256(v.read_bytes()).hexdigest() for k,v in ex.items()} for t,ex in exe.items()},
      "rustc":{t:subprocess.check_output(["rustc","+"+t,"-Vv"],text=True).strip() for t in tools},
      "mount":subprocess.check_output(["findmnt","-n","-o","SOURCE,FSTYPE,TARGET","-T",str(dir_)],text=True).strip()}
(out/"meta.json").write_text(json.dumps(meta,indent=2))
matched=["fixed","plain","candidate"]
for i in range(3):
    for pos,k in enumerate(matched[i:]+matched[:i]):
        for tc in tools[::1 if (i+pos)%2==0 else -1]:
            env=os.environ.copy()
            path=dir_/f"matched-{tc}-{k}-{i}"
            rss=out/f"matched-{tc}-{k}-{i}.rss"
            command=[str(exe[tc][k]),
                     "io_uring","read",str(64<<20),"1","contiguous","3","100","2",str(path)]
            p=subprocess.run(["/usr/bin/time","-f","%M","-o",str(rss),*command],env=env,capture_output=True,text=True)
            if p.returncode: print(p.stderr,flush=True); p.check_returncode()
            r=json.loads(p.stdout)
            assert r["verified"] and r["direct_io"] and r["physical_owners"]==1 and r["payload_bytes"]==64<<20
            assert len(r["samples_ns"])==len(r["cpu_ns"])==100 and "owner-phases " not in p.stderr
            r.update(tc=tc,mode=k,i=i,position=pos,rss=int(rss.read_text()),boot=boot)
            (out/f"matched-{tc}-{k}-{i}.json").write_text(json.dumps(r))
            path.unlink(missing_ok=True)
            print(tc,k,i,"GiB/s",round(0.0625e9*100/sum(r["samples_ns"]),4),"p99_ms",round(sorted(r["samples_ns"])[98]/1e6,4),"cpu_ns/B",round(sum(r["cpu_ns"])/(100*(64<<20)),4),"rss_KiB",r["rss"],flush=True)
    print("finished matched round",i+1,flush=True)
