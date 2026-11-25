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
exe={t:{k:Path((outroot/f"{t}-{k}.exe").read_text().strip()) for k in ["plain","probe"]} for t in tools}
meta={"boot":boot,"kernel":os.uname().release,"trials":6,"samples":20,"warmup":3,"base":"ea265fdecbea48e4f05dff32314f966251943f52",
      "head":subprocess.check_output(["git","rev-parse","HEAD"],text=True).strip(),
      "binary_sha256":{t:{k:sha256(v.read_bytes()).hexdigest() for k,v in ex.items()} for t,ex in exe.items()},
      "rustc":{t:subprocess.check_output(["rustc","+"+t,"-Vv"],text=True).strip() for t in tools},
      "mount":subprocess.check_output(["findmnt","-n","-o","SOURCE,FSTYPE,TARGET","-T",str(dir_)],text=True).strip()}
(out/"meta.json").write_text(json.dumps(meta,indent=2))
stats={}
for i in range(6):
    for pos,label in enumerate(labels[i:]+labels[:i]):
        for tc in tools[::1 if (i+pos)%2==0 else -1]:
            env=os.environ.copy()
            if label in ["natural","pooled"]: env["ORBITE_LARGE_PHASE"]="trace"
            if label in ["prefault","pooled-touch"]:env["ORBITE_LARGE_PHASE"]="prefault"
            if label.startswith("pooled"):env["ORBITE_LARGE_PHASE_POOL"]="1"
            path=dir_/f"payload-{tc}-{label}-{i}"
            cmd=[str(exe[tc]["plain" if label=="parent" else "probe"]),"io_uring","read",str(64<<20),"1","contiguous","3","20","2",str(path)]
            rss=out/f"{tc}-{label}-{i}.rss"
            p=subprocess.run(["/usr/bin/time","-f","%M","-o",str(rss),*cmd],env=env,capture_output=True,text=True)
            if p.returncode:
                print(p.stderr,flush=True)
                p.check_returncode()
            r=json.loads(p.stdout)
            assert r["verified"] and r["direct_io"] and r["physical_owners"]==1 and r["payload_bytes"]==64<<20
            assert len(r["samples_ns"])==20 and len(r["cpu_ns"])==20
            records=[]
            for line in p.stderr.splitlines():
                if not line.startswith("owner-phases "): continue
                assert line.startswith(f"owner-phases {64<<20} ")
                nums=re.findall(r"\[(\d+), (\d+), (\d+)\]",line)
                assert len(nums)==5
                records.append(dict(zip(stages,[[int(v) for v in a] for a in nums])))
            assert len(records)==(23 if label not in ["parent","disabled"] else 0)
            r.update(tc=tc,mode=label,i=i,pos=pos,rss=int(rss.read_text()),boot=boot,owner=records)
            (out/f"{tc}-{label}-{i}.json").write_text(json.dumps(r))
            times=[sum(r["samples_ns"])/len(r["samples_ns"])/1e6,sum(r["cpu_ns"])/len(r["cpu_ns"])/1e6,r["rss"]/1024]
            if records: times.extend(median(x[s][0] for x in records[3:])/1e6 for s in stages)
            stats.setdefault((tc,label),[]).append(times)
    print("finished order",i+1,flush=True)
for k,v in stats.items():
    print(k,"wall_ms/cpu_ms/peak_MiB and stages wall_ms",*[round(median(x),4) for x in zip(*v)],flush=True)

