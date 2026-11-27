"""Replay frozen-caller qualification and isolated stage measurements from the original logs."""
from pathlib import Path
from statistics import median
import json,re,math,sys
from single_stages import phase_rows,verify_residency,meanrow,medrow
SIZE=1<<26
GROUPS=("fixed","hot","tent","reclaim","old_reclaim","populate","warm64","previous","candidate")
SINGLE=("hot","reclaim","populate")
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
def base(mode):return {'ORBITE_DIAG_SPLIT_MODE':'one','ORBITE_DIAG_SINGLE_IDLE':mode}
def replay(path,kind):
    root=Path(__file__).resolve().parent;plan=read(root/'single_plan.json');meta=read(path/'meta.json')
    check(meta['sources']==plan and meta['mount'].split()[0]=='ext4' and 'x86_64' in meta['cpu'],'sources and real host')
    check(meta['caller_sha256']=='4ba461cc4995a1a8e834c946039d04878942e6046977e3797cd6280589cc24bb' and meta['tent_revision']=='e1ea5d31bb5e46ff2df9f90e7c2fbaf52fcdee8c','frozen sources')
    check(meta['source_parents']['old_reclaim']==[plan['warm64']],'historic single reclaim/hot lineage')
    result=dict(runner_slot=meta['runner_slot'],compiler=('1.85.0' if meta['compiler'].startswith('rustc 1.85.0 ') else 'stable'),boot_id=meta['boot_id'],source=plan)
    if kind=='profile':
        checks=read(path/'resident-pages.json');mid={};public=read(path/'profile.json')
        for g in SINGLE:
            _,err=raw(path,'residency-'+g,'probe',base(g)|{'ORBITE_DIAG_SINGLE_RESIDENCY':'1'},3,meta)
            check(verify_residency(err,g,3)==checks[g],'residency')
            trials=[]
            for t in (1,2,3):
                tag=f'stages-{g}-{t}';r,log=raw(path,tag,'clock',base(g)|{'ORBITE_DIAG_SPLIT_PHASES':'1'},100,meta)
                vals,h=phase_rows(r,log,100);v=meanrow(vals);saved=read(path/(tag+'.json'))
                check(h==saved['header'] and all(equal(x,saved['mean'][k]) for k,x in v.items()),'per-process stage replay')
                trials.append(v)
            mid[g]=medrow(trials);check(all(equal(x,public['medians'][g][k]) for k,x in mid[g].items()),'stage median')
        elapsed={g:{k:mid[g][k]-mid['hot'][k] for k in mid[g]} for g in ('reclaim','populate')}
        perf=read(path/'perf-status.json')
        check(bool(perf['perf']) or perf.get('availability'),'function sampler capability report')
        for g,v in perf['modes'].items():
            check(v.get('condition') and (v['condition']!='observed' or v['samples_in_verified_windows']>0),'function sample evidence')
        return result|{'profile_samples':900,'resident_reads':checks,'medians':mid,'minus_hot':elapsed,'function_samples':{k:v.get('condition') for k,v in perf['modes'].items()},'perf_availability':perf.get('availability')}
    checks=read(path/'controls.json');spec=read(root/'single_controls.json')
    check(set(checks)==set(spec),'all fault groups')
    for g,tests in spec.items():
        check(set(checks[g])=={t['name'] for t in tests},'test set')
        for t in tests:
            log=(path/f"fault-{g}-{t['name']}.log").read_text()
            check('test '+t['full']+' ... ' in log and 'test result: ok. 1 passed; 0 failed; 0 ignored;' in log and 'test result: FAILED' not in log,'exact test receipt')
            for pat in t.get('patterns',[]):check(re.search(pat,log,re.M),'missing fault receipt')
            for pat,n in t.get('occurrences',{}).items():check(len(re.findall(pat,log,re.M))==n,'fault repetition')
    check(any('route-control' in l and re.search(r'O_DIRECT[^=]*= [0-9]+',l) for l in (path/'tent-route.log').read_text().splitlines()),'TENT direct route')
    groups=tuple(g for g in GROUPS if g!='candidate' or plan.get('candidate'))
    values={g:[] for g in groups};public=read(path/'summary.json')
    for idx,order in enumerate((groups,groups[::-1],groups[2:]+groups[:2]),1):
        for pos,g in enumerate(order):
            tag=f'timed-{g}-{idx}';src='probe' if g in SINGLE else g
            opts=base(g) if g in SINGLE else {'ORBITE_DIAG_SPLIT_MODE':'one'} if g=='previous' else {'MC_RPC_CLIENT_IO_THREADS':'2','MC_TE_RPC_CLIENT_IO_THREADS':'2'} if g=='tent' else {}
            row,stderr=raw(path,tag,src,opts,100,meta)
            check(not any(x in stderr for x in ('SPLIT_PHASE ','SINGLE_RESIDENCY ','CALLER_SAMPLE ','CALLER_CLOCK ','TASK_CLOCK_WINDOW ','SCALAR_PHASE ')),'traces in formal run')
            tm=(path/(tag+'.time')).read_text().split();check(len(tm)==5 and int(tm[0])>0 and int(tm[1])>0,'RSS or input blocks')
            ns=row['samples_ns'];m=dict(zip(LIMIT,(SIZE*100e9/sum(ns)/2**30,sorted(ns)[98]/1e6,sum(row['cpu_ns'])/(100*SIZE),int(tm[0])/1024)))
            saved=read(path/(tag+'.json'));check((saved['trial'],saved['position'],saved['group'])==(idx,pos,g) and all(equal(x,saved['metrics'][k]) for k,x in m.items()),'timed trial')
            values[g].append(m)
    mid={g:medrow(v) for g,v in values.items()};verdict={}
    for g in groups:
        check(all(equal(x,public['timed_medians'][g][k]) for k,x in mid[g].items()),'untraced medians')
        d=mid['warm64']['peak_rss_MiB']-mid[g]['peak_rss_MiB'];ok=d>=32
        ratios={}
        for b in ('fixed','tent'):
            ratios[b]={}
            for k,lim in LIMIT.items():
                q=mid[g][k]/mid[b][k];passes=q>=lim if k=='goodput_GiB_s' else q<=lim;ratios[b][k]={'ratio':q,'passed':passes};ok=ok and passes
                old=public['results'][g]['vs_references'][b][k];check(equal(old['ratio'],q) and old['passed']==passes,'gate')
        check(ok==public['results'][g]['passed'] and equal(d,public['results'][g]['rss_drop_MiB']),'RSS/thresholds')
        verdict[g]=dict(passed=ok,rss_drop_MiB=d,vs_references=ratios)
    return result|{'formal_samples':len(groups)*300,'timed_medians':mid,'results':verdict,'candidate':plan.get('candidate')}
if __name__=='__main__':print(json.dumps(replay(Path(sys.argv[1]),sys.argv[2]),indent=2))
