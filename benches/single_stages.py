"""Read public caller, rusage, wall and thread clocks from the eight non-overlapping owner stages."""
import json
from statistics import mean,median
STAGES=('preflight','population','ring_setup','first_enter','second','drain','publish','retire')
def require(c,error):
    if not c:raise ValueError(error)
def records(s,p):return [json.loads(ln[len(p)+1:]) for ln in s.splitlines() if ln.startswith(p+' ')]
def meanrow(rows):return {k:mean(r[k] for r in rows) for k in rows[0]}
def medrow(rows):return {k:median(r[k] for r in rows) for k in rows[0]}
def phase_rows(raw,stderr,n):
    h,=records(stderr,'CALLER_CLOCK');a=records(stderr,'SPLIT_PHASE');c=records(stderr,'CALLER_SAMPLE')
    require(len(a)==len(c)==3+n and h['before_ns']<=h['checked_ns']<=h['after_ns'] and h['pid']==h['caller_tid'] and h['pthread_clock']==((~h['caller_tid']<<3)|6),'dynamic caller clock/header/count')
    require('SINGLE_RESIDENCY ' not in stderr,'resident tracing in timing groups')
    req=[f'{s}_{clock}_ns' for s in STAGES for clock in ('cpu','wall','thread','caller')]
    req += [f'{s}_{clock}_{v}' for s in STAGES for clock in ('process','issuer') for v in ('user_us','system_us','minflt','majflt')]
    samples=[]
    for i,(x,y) in enumerate(zip(a,c)):
        require(x['index']==y['index']==i and x['pid']==y['pid']==h['pid'] and x['caller_tid']==y['caller_tid']==h['caller_tid'] and x['owner_tid']!=x['caller_tid'] and x['owner_tid']>0 and x['pthread_clock']==h['pthread_clock'] and x['pieces']==1,'wrong owner identity or one-SQE shape')
        require(all(type(x[k]) is int and x[k]>=0 for k in req),'invalid stage accounting')
        if i<3:continue
        require(y['public_cpu_ns']==raw['cpu_ns'][i-3] and y['public_wall_ns']==raw['samples_ns'][i-3],'mismatched public samples')
        d={k:x[k] for k in req}
        for k in ('public_cpu_ns','public_wall_ns','caller_cpu_ns','public_user_us','public_system_us'):d[k]=y[k]
        for k in ('cpu','wall','caller'):
            public='caller_cpu_ns' if k=='caller' else 'public_'+k+'_ns'
            d['outside_'+k+'_ns']=d[public]-sum(x[f'{s}_{k}_ns'] for s in STAGES)
        for what in ('user_us','system_us'):
            d['outside_process_'+what]=d['public_'+what]-sum(x[f'{s}_process_{what}'] for s in STAGES)
        for s in STAGES:d[s+'_remaining_ns']=d[s+'_cpu_ns']-d[s+'_thread_ns']-d[s+'_caller_ns']
        samples.append(d)
    return samples,h
def verify_residency(stderr,mode,n):
    q=records(stderr,'SINGLE_RESIDENCY')
    require(len(q)==n+2 and 'SPLIT_PHASE ' not in stderr,'residency attempt count or phase timing')
    pair={'hot':(36<<20,36<<20),'reclaim':(0,0),'populate':(0,36<<20)}[mode]
    require(all((r['mode'],r['bytes'],r['before'],r['after'])==(mode,36<<20,*pair) for r in q),'unexpected resident charge for '+mode)
    return len(q)
