from pathlib import Path
import json,sys
def read(p):return json.loads(p.read_text())
def collect(b):
    slots=("stable-first","msrv-first");tcs=("stable","1.85.0")
    boots=set();cells={};source=None
    for s in slots:
        keys=set()
        for tc in tcs:
            d=b/f"single-formal-{s}-{tc}";q=read(d/"recalculated.json");m=read(d/"meta.json")
            if (q['runner_slot'],q['compiler'],q['boot_id'])!=(s,tc,m['boot_id']):raise ValueError('matrix mismatch')
            if source and q['source']!=source:raise ValueError('different sources')
            source=q['source']
            keys.add((m['boot_id'],m['host'],m['mount'],m['executables']['tent']['sha256'],m['workflow_revision']))
            cells[s+'/'+tc]=dict(fixed_GiB_s=q['timed_medians']['fixed']['goodput_GiB_s'],tent_GiB_s=q['timed_medians']['tent']['goodput_GiB_s'],candidate_pass=q['results'].get('candidate',{}).get('passed'))
        if len(keys)!=1 or next(iter(keys))[0] in boots:raise ValueError('incompatible boots or frozen binary')
        boots.add(next(iter(keys))[0])
    return dict(cells=cells,production_candidate=source.get('candidate'),candidate_pass_all=all(v['candidate_pass'] for v in cells.values()) if source.get('candidate') else None)
if __name__=='__main__':
    r=collect(Path(sys.argv[1]));print(json.dumps(r,indent=2))
    if r['candidate_pass_all'] is False:raise SystemExit('candidate did not meet original limits on all four formal cells')
