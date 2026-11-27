"""Replay frozen-caller qualification and isolated stage measurements from the original logs."""
from pathlib import Path
from statistics import median
import json,re,math,sys
def medrow(rows):return {k:median(r[k] for r in rows) for k in rows[0]}
SIZE=1<<26
GROUPS=("fixed","parent","tent","candidate","warm64","old_reclaim","distributed","distributed_half")
LIMIT={"goodput_GiB_s":.95,"p99_ms":1.05,"cpu_ns_per_byte":1.10,"peak_rss_MiB":1.10}
def read(p):return json.loads(p.read_text())
def check(x,msg):
    if not x:raise ValueError(msg)
def equal(a,b):return math.isclose(a,b,rel_tol=1e-10,abs_tol=1e-6)
def raw(path,name,src,opts,n,meta):
    row=read(path/(name+'.stdout'));cmd=read(path/(name+'.command.json'));log=(path/(name+'.stderr')).read_text()
    check(row['verified'] and row['payload_bytes']==SIZE and (src=='tent' or (row['direct_io'] and row['physical_owners']==1)),'byte check, direct route or owner count')
    check(n==len(row['cpu_ns'])==len(row['samples_ns']) and all(type(v)is int and v>0 for v in row['samples_ns']+row['cpu_ns']),'sample count')
    exe=meta['executables'][src];argv=cmd['command'];i=argv.index(exe['path'])
    check(argv[i+1:-1]==['io_uring','read',str(SIZE),'1','contiguous','3',str(n),'2'] and Path(argv[-1]).name==name,'public command')
    check(cmd['source']==src and cmd['boot_id']==meta['boot_id'] and cmd['env_overrides']==opts and cmd['measured_executable_sha256']==exe['sha256'],'source/command')
    return row,log
def replay(path,kind):
    root=Path(__file__).resolve().parent;plan=read(root/'readv2_plan.json');meta=read(path/'meta.json')
    check(meta['sources']==plan and meta['mount'].split()[0]=='ext4' and 'x86_64' in meta['cpu'],'sources and real host')
    check(meta['caller_sha256']=='4ba461cc4995a1a8e834c946039d04878942e6046977e3797cd6280589cc24bb' and meta['tent_revision']=='e1ea5d31bb5e46ff2df9f90e7c2fbaf52fcdee8c','frozen sources')
    check(meta['source_parents']['old_reclaim']==[plan['warm64']],'historic single reclaim/hot lineage')
    check(meta['source_parents']['candidate']==[plan['parent']] and bool(meta['parent_commits_above_pr']) and meta['parent_commits_above_pr'][0]==plan['parent'],'production parent and candidate lineage')
    for name in ('distributed','distributed_half'):
        if plan.get(name):check(meta['source_parents'][name]==[plan['parent' if name=='distributed' else 'distributed']],'distribution lineage '+name)
    result=dict(runner_slot=meta['runner_slot'],compiler=('1.85.0' if meta['compiler'].startswith('rustc 1.85.0 ') else 'stable'),boot_id=meta['boot_id'],source=plan)
    checks=read(path/'controls.json');spec=read(root/'readv2_controls.json');failures=read(path/'control-failures.json')
    check(set(checks)==set(spec),'all fault groups')
    for g,tests in spec.items():
        check(set(checks[g])=={t['name'] for t in tests},'test set')
        for t in tests:
            log=(path/f"fault-{g}-{t['name']}.log").read_text()
            command=read(path/f"fault-{g}-{t['name']}.json");failed=t['name'] in failures.get(g,{})
            try:
                check(command['returncode']==0,'nonzero exact-test exit')
                check('test '+t['full']+' ... ' in log and 'test result: ok. 1 passed; 0 failed; 0 ignored;' in log and 'test result: FAILED' not in log,'exact test receipt')
                for pat in t.get('patterns',[]):check(re.search(pat,log,re.M),'missing fault receipt')
                for pat,n in t.get('occurrences',{}).items():check(len(re.findall(pat,log,re.M))==n,'fault repetition')
                if g in ('distributed','distributed_half') and t['name']=='drained_private_slot_reclaims_pages_in_each_quarter':
                    matches=re.findall(r'discarded bytes by quarter, residue (\d+): (\[[0-9, ]+\])',log)
                    check([int(residue) for residue,raw in matches]==[0,4096],'distributed control residues')
                    for _,raw_quarters in matches:
                        v=json.loads(raw_quarters)
                        check(len(v)==4 and sum(v)==(36 if g=='distributed' else 32)<<20 and all((6<<20)<=a<=(12<<20) for a in v),'wrong distribution of returned resident pages')
            except ValueError:
                check(failed,'missing hosted fault failure');continue
            check(not failed,'false hosted fault failure')
    check(any('route-control' in l and re.search(r'O_DIRECT[^=]*= [0-9]+',l) for l in (path/'tent-route.log').read_text().splitlines()),'TENT direct route')
    groups=tuple(g for g in GROUPS if g not in ('distributed','distributed_half') or plan.get(g))
    values={g:[] for g in groups};public=read(path/'summary.json')
    for idx,order in enumerate((groups,groups[::-1],groups[2:]+groups[:2]),1):
        for pos,g in enumerate(order):
            tag=f'timed-{g}-{idx}';src=g
            opts={'MC_RPC_CLIENT_IO_THREADS':'2','MC_TE_RPC_CLIENT_IO_THREADS':'2'} if g=='tent' else {}
            row,stderr=raw(path,tag,src,opts,100,meta)
            check(not any(x in stderr for x in ('SPLIT_PHASE ','SINGLE_RESIDENCY ','CALLER_SAMPLE ','CALLER_CLOCK ','TASK_CLOCK_WINDOW ','SCALAR_PHASE ')),'traces in formal run')
            tm=(path/(tag+'.time')).read_text().split();check(len(tm)==5 and int(tm[0])>0 and int(tm[1])>0,'RSS or input blocks')
            ns=row['samples_ns'];m=dict(zip(LIMIT,(SIZE*100e9/sum(ns)/2**30,sorted(ns)[98]/1e6,sum(row['cpu_ns'])/(100*SIZE),int(tm[0])/1024)))
            saved=read(path/(tag+'.json'));check((saved['trial'],saved['position'],saved['group'])==(idx,pos,g) and all(equal(x,saved['metrics'][k]) for k,x in m.items()),'timed trial')
            values[g].append(m)
    mid={g:medrow(v) for g,v in values.items()};verdict={}
    for g in groups:
        check(all(equal(x,public['timed_medians'][g][k]) for k,x in mid[g].items()),'untraced medians')
        d=mid['warm64']['peak_rss_MiB']-mid[g]['peak_rss_MiB'];ok=d>=32 and not any(k in failures for k in (g,'parent','old_reclaim'))
        ratios={}
        for b in ('fixed','tent'):
            ratios[b]={}
            for k,lim in LIMIT.items():
                q=mid[g][k]/mid[b][k];passes=q>=lim if k=='goodput_GiB_s' else q<=lim;ratios[b][k]={'ratio':q,'passed':passes};ok=ok and passes
                old=public['results'][g]['vs_references'][b][k];check(equal(old['ratio'],q) and old['passed']==passes,'gate')
        check(ok==public['results'][g]['passed'] and equal(d,public['results'][g]['rss_drop_MiB']),'RSS/thresholds')
        verdict[g]=dict(passed=ok,rss_drop_MiB=d,vs_references=ratios)
    return result|{'control_failures':failures,'formal_samples':len(groups)*300,'timed_medians':mid,'results':verdict,'candidate':plan.get('candidate')}
if __name__=='__main__':print(json.dumps(replay(Path(sys.argv[1]),sys.argv[2]),indent=2))
