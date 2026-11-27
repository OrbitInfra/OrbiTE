"""Run on one x86_64 Linux boot. This diagnostic driver never publishes code."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
from statistics import median
import subprocess
import sys

SIZE = 64 << 20
COLD = 36 << 20
LIMITS = {"goodput_GiB_s": (.95, "min"), "p99_ms": (1.05, "max"), "cpu_ns_per_byte": (1.10, "max"), "peak_rss_MiB": (1.10, "max")}
PHASES = ("setup","first_enter","second","drain","publish","retire")
BUILDS=("fixed","warm64","old_reclaim","previous","probe","candidate")
CALLER = "0d57f95fe70468e8bd1fa9f886cf9b03b46ed6a5"
FROZEN = "e1ea5d31bb5e46ff2df9f90e7c2fbaf52fcdee8c"
FAULTS = ("large_direct_read_after_truncate_publishes_only_the_confirmed_prefix", "large_direct_read_with_negative_cqe_preserves_the_entire_destination", "large_direct_read_uses_device_alignment_when_huge_alignment_is_unavailable")
REUSE = "large_direct_read_reuses_only_one_private_prefix_and_publishes_exact_short_cqes"
SPLIT_FAULTS = ("direct_read_split_keeps_the_consumed_prefix_owned",
                "partial_direct_read_with_failed_unmap_keeps_every_owner",
                "large_direct_read_with_failed_scratch_unmap_retains_admission_and_mapping")
TEST_MODULE = "transport::io_uring::tests::inline::group::"


def require(condition, message):
    if not condition: raise ValueError(message)


def read(path): return json.loads(path.read_text())
def save(path, data): path.write_text(json.dumps(data, indent=2, allow_nan=False)+"\n")
def sha(path):
    with path.open("rb") as stream: return hashlib.file_digest(stream, "sha256").hexdigest()
def output(*args, cwd=None):
    return subprocess.check_output([str(a) for a in args], cwd=cwd, text=True).strip()
def env(**overrides):
    clean = {k:v for k,v in os.environ.items() if not k.startswith(("ORBITE_DIAG_", "ORBITE_LARGE_", "ORBITE_BENCH_", "MC_", "MOONCAKE_", "TENT_")) and k not in ("LD_PRELOAD", "LD_LIBRARY_PATH")}
    return clean | overrides
def checked(row, n, group):
    require(row.get("verified") is True and row["payload_bytes"] == SIZE, f"{group}: wrong payload or guard")
    require(group == "tent" or (row["direct_io"] is True and row["physical_owners"] == 1), f"{group}: wrong OrbiTE route or owner count")
    for k in ("cpu_ns", "samples_ns"):
        require(len(row[k]) == n and all(type(v) is int and v > 0 for v in row[k]), f"{group}: wrong {k}")


def metrics(row, rss):
    n=len(row["cpu_ns"])
    return {"goodput_GiB_s": n*SIZE*1e9/(sum(row["samples_ns"])*2**30), "p99_ms": sorted(row["samples_ns"])[98]/1e6,
            "cpu_ns_per_byte": sum(row["cpu_ns"])/(n*SIZE), "peak_rss_MiB": rss/1024}


def system(tmp, ext, out, plan, tc):
    require(os.uname().machine=="x86_64", "native x86_64 required")
    mount=output("findmnt","-n","-o","FSTYPE,SOURCE,TARGET,OPTIONS","-T",ext)
    require(mount.split()[0]=="ext4", "real ext4 required")
    m={"boot_id":Path("/proc/sys/kernel/random/boot_id").read_text().strip(), "host":os.uname().nodename, "kernel":os.uname().release,
       "cxx":output("c++","--version"), "compiler":output("rustc","+"+tc,"-Vv"), "mount":mount,
       "cpu":output("lscpu"), "affinity":sorted(os.sched_getaffinity(0)), "workflow_revision":output("git","rev-parse","HEAD"), "sources":plan}
    return m


def build(root, tmp, ext, out, plan, tc):
    from run import tent_build_command, check_caller_dep_info
    for ref in (*BUILDS,"caller"):
        if plan.get(ref):
            output("git","worktree","add","--quiet","--detach",tmp/ref,plan[ref])
            require(output("git","rev-parse","HEAD",cwd=tmp/ref)==plan[ref] and not output("git","status","--porcelain",cwd=tmp/ref), "unclean checkout "+ref)
    caller=tmp/"caller"/"benches"
    require(plan["caller"]==CALLER and sha(caller/"transfer.rs")=="4ba461cc4995a1a8e834c946039d04878942e6046977e3797cd6280589cc24bb" and sha(caller/"tent.cpp")=="4d84ec404d73d6ded215509a9a20f57741a34ec1de4106521bfaf891ade34222","caller mismatch")
    shared=Path(os.environ["RUNNER_TEMP"])
    require(output("git","rev-parse","HEAD",cwd=shared/"oracle"/"reference")==FROZEN,"wrong TENT source")
    exe_t=shared/"tent-bench"
    tent_receipt=shared/"tent-bench-build.json"
    if not tent_receipt.exists():
        tent_cmd=tent_build_command(shared/"oracle",caller.parent,shared)
        subprocess.run(tent_cmd,check=True)
        save(tent_receipt,{"command":tent_cmd,"sha256":sha(exe_t)})
    built=read(tent_receipt)
    require(sha(exe_t)==built["sha256"],"host TENT executable changed")
    tent_cmd=built["command"]
    meta=system(tmp,ext,out,plan,tc)
    meta["runner_slot"]=os.environ["RUNNER_SLOT"]
    meta["source_parents"]={k:output("git","show","-s","--format=%P",plan[k],cwd=tmp/k).split() for k in BUILDS if plan.get(k)}
    meta["caller_sha256"]=sha(caller/"transfer.rs");meta["tent_caller_sha256"]=sha(caller/"tent.cpp");meta["tent_revision"]=FROZEN
    meta["executables"]={"tent":{"path":str(exe_t),"sha256":sha(exe_t),"build":tent_cmd}}
    for label in BUILDS:
        if not plan.get(label): continue
        command=["cargo","+"+tc,"build","--release","--locked","--features","io-uring","--bench","transfer","--message-format=json"]
        build_env={"RUSTC_WORKSPACE_WRAPPER":str(caller/"build_caller.py"),"CARGO_TARGET_DIR":str(tmp/("target-"+label))}
        p=subprocess.run(command,check=True,cwd=tmp/label,env=env(**build_env),stdout=subprocess.PIPE,text=True)
        exe_rows=[r["executable"] for line in p.stdout.splitlines() if (r:=json.loads(line)).get("target",{}).get("name")=="transfer" and r.get("executable")]
        require(len(exe_rows)==1, f"{label} missing executable")
        exe=Path(exe_rows[0]).resolve(strict=True)
        require(exe.is_relative_to(tmp/("target-"+label)),"wrong build directory")
        check_caller_dep_info(exe.with_suffix(".d").read_text(),exe,caller/"transfer.rs")
        meta["executables"][label]={"path":str(exe),"sha256":sha(exe),"build":command,"environment":build_env}
    save(out/"meta.json",meta)


def run_one(out, ext, meta, source, label, sampling, options, receipt_time=False):
    path=ext/label
    require(not path.exists(),"stale fixture "+label)
    exe=Path(meta["executables"][source]["path"])
    args=[str(exe),"io_uring","read",str(SIZE),"1","contiguous","3",str(sampling),"2",str(path)]
    options=options|({"MC_RPC_CLIENT_IO_THREADS":"2","MC_TE_RPC_CLIENT_IO_THREADS":"2"} if source=="tent" else {})
    cmd=["/usr/bin/time","-f","%M %I %U %S %e","-o",str(out/(label+".time")),"--",*args] if receipt_time else args
    run=subprocess.run(cmd,env=env(**options),capture_output=True,text=True)
    (out/(label+".stdout")).write_text(run.stdout);(out/(label+".stderr")).write_text(run.stderr)
    save(out/(label+".command.json"),{"command":cmd,"measured_executable_sha256":meta["executables"][source]["sha256"],"source":source,"env_overrides":options,"boot_id":meta["boot_id"]})
    run.check_returncode()
    row=json.loads(run.stdout);checked(row,sampling,source)
    require(path.exists(),"caller did not leave its file fixture")
    path.unlink()
    return row,run.stderr



SETTINGS = {
    "hot": ("probe", {"ORBITE_DIAG_SPLIT_MODE":"one","ORBITE_DIAG_SINGLE_IDLE":"hot"}),
    "reclaim": ("probe", {"ORBITE_DIAG_SPLIT_MODE":"one","ORBITE_DIAG_SINGLE_IDLE":"reclaim"}),
    "populate": ("probe", {"ORBITE_DIAG_SPLIT_MODE":"one","ORBITE_DIAG_SINGLE_IDLE":"populate"}),
    "previous": ("previous", {"ORBITE_DIAG_SPLIT_MODE":"one"}),
}
GROUPS = ("fixed","hot","tent","reclaim","old_reclaim","populate","warm64","previous","candidate")
TRACE_TAGS = ("CALLER_CLOCK ","CALLER_SAMPLE ","SPLIT_PHASE ","TASK_CLOCK_WINDOW ","SINGLE_RESIDENCY ","SCALAR_PHASE ","PREFLT ")

def order(groups,turn):
    return (groups,groups[::-1],groups[2:]+groups[:2])[turn]

def formal_groups(plan):
    return tuple(g for g in GROUPS if g!="candidate" or plan.get("candidate"))

def reference_receipt(ext,out,meta):
    route=ext/"tent-route-control"
    cmd=["strace","-f","-yy","-e","trace=openat","-o",str(out/"tent-route.log"),meta["executables"]["tent"]["path"],"io_uring","read",str(SIZE),"1","contiguous","1","1","2",str(route)]
    row=json.loads(subprocess.check_output(cmd,text=True,env=env(MC_RPC_CLIENT_IO_THREADS="2",MC_TE_RPC_CLIENT_IO_THREADS="2")))
    checked(row,1,"tent")
    require(any(str(route) in line and re.search(r'O_DIRECT[^=]*= [0-9]+',line) for line in (out/"tent-route.log").read_text().splitlines()),"TENT did not use O_DIRECT")
    route.unlink()

def controls(root,tmp,ext,out,plan,tc):
    meta=read(out/"meta.json")
    reference_receipt(ext,out,meta)
    specs=read(root/"benches/single_controls.json")
    executed={}
    for group,profile in specs.items():
        if group=="candidate" and not plan.get("candidate"):continue
        src,opts=SETTINGS.get(group,(group,{}))
        executed[group]=[]
        for test in profile:
            name=test["name"];full=test["full"]
            cfg={"ORBITE_REQUIRE_IO_URING":"1","ORBITE_EXT4_DIR":str(ext),"ORBITE_TMPFS_DIR":"/dev/shm","CARGO_TARGET_DIR":str(tmp/("target-"+src)),**opts,**test.get("env",{})}
            cmd=["cargo","+"+tc,"test","--release","--locked","--features","io-uring","--lib","--","--exact",full,"--nocapture","--test-threads=1"]
            proc=subprocess.run(cmd,cwd=tmp/src,env=env(**cfg),stdout=subprocess.PIPE,stderr=subprocess.STDOUT,text=True)
            (out/f"fault-{group}-{name}.log").write_text(proc.stdout)
            save(out/f"fault-{group}-{name}.json",{"command":cmd,"environment":cfg,"returncode":proc.returncode,"patterns":test.get("patterns",[])})
            proc.check_returncode()
            ends=re.findall(r"test result: (\w+)\. (\d+) passed; (\d+) failed; (\d+) ignored;",proc.stdout)
            require(ends and all(q==("ok","1","0","0") for q in ends) and re.search("^test "+re.escape(full)+r" \.\.\. ",proc.stdout,re.M),"test did not execute: "+full)
            for pattern in test.get("patterns",[]):require(re.search(pattern,proc.stdout,re.M),"missing fault receipt: "+pattern)
            for pattern,count in test.get("occurrences",{}).items():
                require(len(re.findall(pattern,proc.stdout,re.M))==count,"wrong fault coverage: "+name)
            executed[group].append(name)
    save(out/"controls.json",executed)

def compare_to(m,b):
    ratios={}
    for k,(bound,direction) in LIMITS.items():
        ratio=m[k]/b[k]
        ratios[k]={"ratio":ratio,"passed":ratio>=bound if direction=="min" else ratio<=bound}
    return ratios

def measure(root,tmp,ext,out,plan,tc):
    meta=read(out/"meta.json")
    require(meta["boot_id"]==Path("/proc/sys/kernel/random/boot_id").read_text().strip(),"boot changed")
    for g,record in meta["executables"].items():
        require(sha(Path(record["path"]))==record["sha256"],"changed executable: "+g)
    groups=formal_groups(plan);vals={g:[] for g in groups}
    for trial in range(3):
        for position,g in enumerate(order(groups,trial)):
            tag=f"timed-{g}-{trial+1}";src,opts=SETTINGS.get(g,(g,{}))
            row,stderr=run_one(out,ext,meta,src,tag,100,opts,True)
            require(not any(tag in stderr for tag in TRACE_TAGS),"unexpected diagnostic output: "+g)
            t=(out/(tag+".time")).read_text().split()
            require(len(t)==5 and int(t[0])>0 and int(t[1])>0,"missing RSS or input blocks")
            v=metrics(row,int(t[0]));vals[g].append(v)
            save(out/(tag+".json"),{"trial":trial+1,"position":position,"group":g,"metrics":v,"peak_rss_kib":int(t[0]),"input_blocks":int(t[1])})
            print(tag,v,flush=True)
    middle={g:{k:median(r[k] for r in rows) for k in LIMITS} for g,rows in vals.items()}
    answer={}
    for g in groups:
        by={b:compare_to(middle[g],middle[b]) for b in ("fixed","tent")}
        drop=middle["warm64"]["peak_rss_MiB"]-middle[g]["peak_rss_MiB"]
        answer[g]={"vs_references":by,"rss_drop_MiB":drop,"passed":drop>=32 and all(x["passed"] for q in by.values() for x in q.values())}
    save(out/"summary.json",{"runner_slot":meta["runner_slot"],"boot_id":meta["boot_id"],"toolchain":tc,"timed_medians":middle,"limits":LIMITS,"results":answer,"production_candidate":plan.get("candidate")})

def profile(root,tmp,ext,out,plan,tc):
    from single_stages import phase_rows,meanrow,medrow,verify_residency
    from single_perf import observe
    formal=read(out/"meta.json");out=tmp/"profile";out.mkdir(exist_ok=True)
    src=tmp/"probe";target=tmp/"target-probe"
    opts={"CARGO_TARGET_DIR":str(target)}
    cmd=["cargo","+"+tc,"build","--release","--locked","--features","io-uring","--bench","transfer_caller_clock","--message-format=json"]
    proc=subprocess.run(cmd,cwd=src,env=env(**opts),check=True,capture_output=True,text=True)
    paths=[q["executable"] for line in proc.stdout.splitlines() if (q:=json.loads(line)).get("target",{}).get("name")=="transfer_caller_clock" and q.get("executable")]
    require(len(paths)==1,"wrong stage executable")
    exe=Path(paths[0]).resolve(strict=True)
    dep=exe.with_suffix(".d").read_text()
    require(exe.is_relative_to(target) and "benches/diagnostics/transfer_caller_clock.rs" in dep,"dedicated caller is missing")
    (out/"diagnostic-caller.d").write_text(dep)
    meta={**formal,"dedicated_caller_sha256":sha(src/"benches/diagnostics/transfer_caller_clock.rs"),"executables":{**formal["executables"],"clock":{"path":str(exe),"sha256":sha(exe),"build":cmd,"environment":opts}}}
    save(out/"meta.json",meta)
    checks={};groups=("hot","reclaim","populate");values={g:[] for g in groups}
    for g in groups:
        _,mode=SETTINGS[g];label="residency-"+g
        row,err=run_one(out,ext,meta,"probe",label,3,{**mode,"ORBITE_DIAG_SINGLE_RESIDENCY":"1"})
        checks[g]=verify_residency(err,g,3)
    save(out/"resident-pages.json",checks)
    for turn in range(3):
        for g in order(groups,turn):
            _,mode=SETTINGS[g];label=f"stages-{g}-{turn+1}"
            row,err=run_one(out,ext,meta,"clock",label,100,{**mode,"ORBITE_DIAG_SPLIT_PHASES":"1"})
            rows,header=phase_rows(row,err,100);values[g].append(meanrow(rows))
            save(out/(label+".json"),{"header":header,"mean":values[g][-1]})
    save(out/"profile.json",{"medians":{g:medrow(vals) for g,vals in values.items()},"residency":checks,"samples":900})
    observe(out,ext,exe,env)

def main():
    act=sys.argv[1]
    tc=os.environ["JOB_TOOLCHAIN"];require(tc in ("stable","1.85.0"),"compiler")
    root=Path(__file__).resolve().parents[1];base=Path(os.environ["RUNNER_TEMP"])
    tmp=base/("single-"+tc);ext=base/"probe-ext4"/tc;out=tmp/"receipts"
    out.mkdir(exist_ok=True,parents=True);ext.mkdir(exist_ok=True,parents=True)
    {"build":build,"controls":controls,"measure":measure,"profile":profile}[act](root,tmp,ext,out,read(root/"benches/single_plan.json"),tc)
if __name__=="__main__":main()
