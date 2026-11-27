"""Optional Linux perf observation for an isolated, byte-checked single-read caller."""
from collections import Counter
from decimal import Decimal
from pathlib import Path
import json, re, shutil, subprocess

def write(p,o):p.write_text(json.dumps(o,indent=2)+'\n')
def frames(script,windows):
    # perf script prints header "pid/tid   sec.nsec: cpu-clock:" and indented IP frames.
    # Match the header independently from comm, then retain only the selected access windows.
    header=re.compile(r'^\s*(\d+)\s*[/ ]\s*(\d+)\s+([0-9]+\.[0-9]+):\s*(?:[^:]+:)?\s*cpu-clock[^:]*:(.*)$')
    # Some perf revisions omit the event's numeric prefix or default to "cpu-clock:".
    counts=Counter();by=Counter();byself=Counter();groups=Counter();active=False;stack=[];total=selected=0
    frame=re.compile(r'^\s*(?:0x)?[0-9a-fA-F]+\s+(.+?)\s+\((.+)\)\s*$')
    def flush():
        nonlocal stack
        if active:
            if stack:
                top=stack[0];byself[top]+=1
                for f in set(stack):by[f]+=1
                groups['symbolized_top' if '[unknown]' not in top and not top.startswith('0x') else 'unresolved_top']+=1
            else:groups['no_decoded_stack']+=1
        stack=[]
    for line in script.splitlines():
        h=header.match(line)
        if h:
            flush();total+=1;pgid,tid,ts,rest=h.groups();at=int(Decimal(ts)*1_000_000_000)
            active=any(int(pgid)==pid and start<=at<=stop for pid,start,stop in windows)
            if active: selected+=1;counts[tid]+=1
            line=rest
        if active:
            m=frame.match(line)
            if m:
                symbol,dso=m.groups();symbol=re.sub(r'\+0x[\da-fA-F]+(?:/0x[\da-fA-F]+)?$','',symbol)
                stack.append(symbol+' ('+dso+')')
    flush()
    return dict(timestamped_samples=total,samples_in_verified_windows=selected,tid_samples=dict(counts),classification=dict(groups),leaf_symbols=byself.most_common(40),inclusive_symbols=by.most_common(40))

def observe(out,ext,exe,base_env):
    perf=shutil.which('perf');state={'perf':perf,'readable_host_settings':{},'modes':{}}
    for k in ('perf_event_paranoid','kptr_restrict'):
        path=Path('/proc/sys/kernel')/k
        if path.exists():state['readable_host_settings'][k]=path.read_text().strip()
    if not perf:
        state['availability']='perf is absent from PATH';write(out/'perf-status.json',state);return
    state['version']=subprocess.run([perf,'version'],text=True,capture_output=True).stdout.strip()
    for g in ('hot','reclaim'):
        body=out/f'perf-{g}';data=out/f'perf-{g}.data';file=ext/f'perf-extra-{g}'
        opts={'ORBITE_DIAG_SPLIT_MODE':'one','ORBITE_DIAG_SINGLE_IDLE':g,'ORBITE_BENCH_TAIL_TRACE':'1'}
        args=[str(exe),'io_uring','read',str(64<<20),'1','contiguous','3','20','2',str(file)]
        cmd=[perf,'record','--clockid','mono','-F','199','-e','cpu-clock','-g','-o',str(data),'--',*args]
        proc=subprocess.run(cmd,capture_output=True,text=True,env=base_env(**opts))
        (out/f'perf-{g}.stdout').write_text(proc.stdout);(out/f'perf-{g}.stderr').write_text(proc.stderr)
        r={'command':cmd,'environment':opts,'returncode':proc.returncode};state['modes'][g]=r
        if proc.returncode:
            r['condition']='perf record failed';r['stderr']=proc.stderr[-3000:]
        else:
            row=json.loads(proc.stdout)
            if not row['verified'] or row['payload_bytes']!=64<<20 or row['direct_io'] is not True or row['physical_owners']!=1 or len(row['samples_ns'])!=20:raise ValueError('invalid perf byte-check fixture')
            pts=[tuple(map(int,m.groups())) for m in re.finditer(r'^ORBITE_SAMPLE (\d+) (\d+) (\d+) (\d+)$',proc.stderr,re.M)]
            if len(pts)!=23 or [e[0] for e in pts]!=list(range(23)) or any(e[1]>=e[2] for e in pts):raise ValueError('invalid perf windows')
            windows=[(pid,a,b) for ix,a,b,pid in pts if ix>=3]
            decode=[perf,'script','--ns','-i',str(data),'-F','pid,tid,time,event,ip,sym,dso']
            decoded=subprocess.run(decode,capture_output=True,text=True)
            (out/f'perf-{g}.script').write_text(decoded.stdout);(out/f'perf-{g}.decode.stderr').write_text(decoded.stderr)
            r.update(decode=decode,decode_returncode=decoded.returncode,windows=windows)
            if decoded.returncode:r['condition']='perf script failed';r['decode_error']=decoded.stderr[-3000:]
            else:
                r.update(frames(decoded.stdout,windows))
                r['condition']='observed' if r['samples_in_verified_windows'] else 'no decodable samples in post-warmup windows'
        if file.is_file():file.unlink()
    write(out/'perf-status.json',state)
