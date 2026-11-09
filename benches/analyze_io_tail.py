"""Summarize diagnostic timelines, including the slowest 1%; never benchmark rows."""
import argparse
import bisect
import collections
import json
from pathlib import Path
import re
import statistics

LINE = re.compile(r'^\s*(.*?)\s+(-?\d+)/(-?\d+)\s+\[\d+\]\s+(\d+)\.(\d+):\s+(\S+):\s+(.*)$')
SWITCH = re.compile(r'.+:(\d+) \[\d+\] (\S+) ==> .+:(\d+) \[\d+\]')
WAKE = re.compile(r'.+:(\d+) \[\d+\] CPU:\d+')
BLOCK = re.compile(r'(\d+,\d+) (\S+) .*?\(\) (\d+) \+ (\d+) .*')


def block_requests(events):
    """Pair across sample boundaries, including reset writeback and kernel workers."""
    pending = collections.defaultdict(collections.deque)
    requests = []
    for at, pid, tid, name, body in events:
        if name not in ('block:block_rq_issue', 'block:block_rq_complete'):
            continue
        m = BLOCK.fullmatch(body)
        if not m:
            raise ValueError('unknown block format: ' + body)
        device, flags, sector, length = m.groups()
        key = m.groups()
        if name == 'block:block_rq_issue':
            # blk_fill_rwbs: prefix F is preflush, suffix F is FUA.
            prefix = flags.startswith('F') and len(flags) > 1 and flags[1] in 'RWDN'
            op, suffix = (flags[1], flags[2:]) if prefix else (flags[0], flags[1:])
            request = dict(issue_ns=at, complete_ns=None, pid=pid, tid=tid, device=device,
                           flags=flags, op=op, bytes=int(length) * 512,
                           sector=int(sector), flush=prefix or op == 'F', fua='F' in suffix)
            pending[key].append(request)
            requests.append(request)
        elif pending[key]:
            pending[key].popleft()['complete_ns'] = at
    return requests


def window_io(requests, begin, end, submit, pid, op, payload):
    # Direct candidates are synchronous data issued by this process after its
    # first SQE. Validate their total; never classify an extra write as payload.
    def direct(r):
        return (r['pid'] == pid and submit <= r['issue_ns'] <= end
                and r['flags'] == op + 'S')
    candidates = [r for r in requests if direct(r)]
    # Depth of this sample's data at the driver issue boundary, separately for
    # each device. This is not device-wide depth or the number of queued SQEs.
    timeline = []
    for r in candidates:
        depth = sum(p['device'] == r['device'] and p['issue_ns'] <= r['issue_ns']
                    and (p['complete_ns'] is None or p['complete_ns'] > r['issue_ns'])
                    for p in candidates)
        timeline.append(dict(r, batch_depth_at_issue=depth))
    devices = {r['device'] for r in candidates}
    for device in devices:
        if sum(r['bytes'] for r in candidates if r['device'] == device) != payload:
            raise ValueError('direct block bytes do not match the measured payload')
    # Report same-device background traffic, not an assertion that every such
    # request belongs to the benchmark. Keep writes already pending at begin.
    overlap = [r for r in requests if r['device'] in devices and r['issue_ns'] <= end
               and (r['complete_ns'] is None or r['complete_ns'] >= begin)]
    other = [r for r in overlap if not direct(r)]
    writes = [r for r in other if r['op'] == 'W']
    return dict(window_direct_blocks=len(candidates), window_other_blocks=len(other),
                window_other_write_bytes=sum(r['bytes'] for r in writes),
                window_other_writes=len(writes),
                window_carry_in_writes=sum(r['issue_ns'] < begin for r in writes),
                window_other_process_writes=sum(r['pid'] != pid for r in writes),
                window_flushes=sum(r['flush'] for r in overlap),
                window_fua=sum(r['fua'] for r in overlap),
                window_discards=sum(r['op'] == 'D' for r in other),
                window_unpaired=sum(r['complete_ns'] is None for r in overlap),
                window_devices=sorted(devices), other_io_timeline=other,
                direct_io_timeline=timeline,
                max_batch_depth=max((r['batch_depth_at_issue'] for r in timeline), default=0))


def blocks(events, pid):
    """Only exact device/operation/sector/length pairs, issued by this process.

    The block trace has no io_uring request identity. These are correlated
    process I/Os (including reset writeback), not proof of a particular SQE.
    Partial completions and requests outside the window remain unmatched.
    """
    pending = collections.defaultdict(list)
    pairs, issues = [], 0
    for at, process, _, name, body in events:
        if name not in ('block:block_rq_issue', 'block:block_rq_complete'):
            continue
        m = BLOCK.fullmatch(body)
        if not m:
            raise ValueError('unknown block format: ' + body)
        key = m.groups()
        if name == 'block:block_rq_issue' and process == pid:
            pending[key].append(at)
            issues += 1
        elif name == 'block:block_rq_complete' and pending[key]:
            pairs.append((pending[key].pop(0), at, key[1]))
    return dict(block_issues=issues, paired_blocks=len(pairs),
                unmatched_block_issues=issues - len(pairs),
                max_block_wait_us=max((end - begin for begin, end, _ in pairs), default=0) / 1000,
                last_block_complete_ns=max((end for _, end, _ in pairs), default=0),
                block_timeline=[dict(issue_ns=a, complete_ns=b, operation=op) for a, b, op in pairs])


def scheduling(events, tid):
    """Separate blocked time from wake-to-run/preempted time, clipped to events."""
    out = wake = None
    blocked, runnable, delays = 0, 0, []
    for at, _, _, name, body in events:
        if name == 'sched:sched_wakeup':
            m = WAKE.fullmatch(body)
            if m and int(m[1]) == tid and wake is None:
                wake = at
        elif name == 'sched:sched_switch':
            m = SWITCH.fullmatch(body)
            if not m:
                raise ValueError('unknown scheduler format: ' + body)
            if int(m[3]) == tid:
                if wake is not None:
                    delays.append(at - wake)
                    runnable += at - wake
                    if out is not None:
                        blocked += wake - out
                elif out is not None:
                    # A missing wake record cannot be classified as blocked time.
                    raise ValueError('missing wake-to-run receipt')
                out = wake = None
            if int(m[1]) == tid and m[2] != 'X':
                out = at
                wake = at if m[2].startswith('R') else None
    return dict(blocked_us=blocked / 1000, runnable_us=runnable / 1000,
                max_wake_to_run_us=max(delays, default=0) / 1000)


def analyze(root):
    summaries = []
    for path in sorted(root.glob('*.windows.json')):
        name = path.name.removesuffix('.windows.json')
        windows = json.loads(path.read_text())
        events = []
        for line in (root / (name + '-events.stdout')).read_text().splitlines():
            m = LINE.fullmatch(line)
            if m:
                _, pid, tid, sec, fraction, event, body = m.groups()
                events.append((int(sec) * 10**9 + int(fraction.ljust(9, '0')), int(pid), int(tid), event, body))
            elif line.strip() and not line.startswith('#'):
                raise ValueError('unknown perf format: ' + line)
        # perf script can repeat an identical record (including its nanosecond
        # timestamp and request pointer). Report that count; it is not another
        # SQE/CQE. Different timestamps or payloads are never collapsed.
        duplicate_records = len(events) - len(set(events))
        events = sorted(set(events))
        times = [e[0] for e in events]
        requests = iter(block_requests(events))
        next_request = next(requests, None)
        active = []
        rows = []
        for w in windows:
            if not w['measured']:
                continue
            begin, end, pid = w['begin_ns'], w['end_ns'], w['pid']
            while next_request is not None and next_request['issue_ns'] <= end:
                active.append(next_request)
                next_request = next(requests, None)
            active = [r for r in active if r['complete_ns'] is None or r['complete_ns'] >= begin]
            es = events[bisect.bisect_left(times, begin):bisect.bisect_right(times, end)]
            own = [e for e in es if e[1] == pid]
            rings = [e for e in own if e[3] == 'io_uring:io_uring_create']
            contexts = {re.search(r'ring (\S+),', e[4])[1] for e in rings}
            io = [e for e in es if e[3].startswith('io_uring:') and any('ring ' + ctx + ',' in e[4] for ctx in contexts)]
            sq = [e for e in io if e[3] in ('io_uring:io_uring_submit_req', 'io_uring:io_uring_submit_sqe')]
            cq = [e for e in io if e[3] == 'io_uring:io_uring_complete']
            if len(rings) != 1 or not sq or len(sq) != len(cq):
                raise ValueError(f'{name}/{w["sample"]}: incomplete ring/SQE/CQE timeline')
            issuer = sq[0][2]
            enters = [e for e in own if e[3] == 'syscalls:sys_enter_io_uring_enter']
            row = dict(sample=w['sample'], begin_ns=begin, end_ns=end, caller=pid, issuer=issuer,
                       latency_us=(end - begin) / 1000, rings=len(rings), sqes=len(sq), cqes=len(cq),
                       enters=len(enters), submitting_enters=sum('to_submit: 0x00000000,' not in e[4] for e in enters),
                       submit_ns=sq[0][0], first_cqe_ns=cq[0][0], last_cqe_ns=cq[-1][0],
                       before_submit_us=(sq[0][0] - begin) / 1000,
                       submit_to_first_cqe_us=(cq[0][0] - sq[0][0]) / 1000,
                       first_to_last_cqe_us=(cq[-1][0] - cq[0][0]) / 1000,
                       last_cqe_to_return_us=(end - cq[-1][0]) / 1000,
                       cross_thread_cqes=sum(e[2] != issuer for e in cq),
                       caller_sleeps=sum(e[2] == pid and e[3] == 'syscalls:sys_enter_clock_nanosleep' for e in own),
                       issuer_sleeps=sum(e[2] == issuer and e[3] == 'syscalls:sys_enter_clock_nanosleep' for e in own),
                       async_work=sum(e[3] == 'io_uring:io_uring_queue_async_work' for e in io))
            row.update(blocks(es, pid))
            _, filesystem, direction, count, _ = name.removesuffix('-settled').removesuffix('-bulk').split('-')
            row.update(window_io(active, begin, end, sq[0][0], pid,
                                 'R' if direction == 'read' else 'W', 4096 * int(count)))
            if filesystem == 'ext4' and not row['window_direct_blocks']:
                raise ValueError('missing ext4 direct block evidence')
            row['last_block_to_cqe_us'] = ((cq[-1][0] - row['last_block_complete_ns']) / 1000
                                         if row['last_block_complete_ns'] else 0)
            # Preserve the exact kernel fields alongside per-sample correlation.
            # iomap records file offsets/length/flags; block bios record device
            # sectors; filemap records invalidated inode/page ranges. They are
            # different address spaces, never interchangeable offsets.
            row['issue_path_timeline'] = [dict(at_ns=e[0], tid=e[2], event=e[3], detail=e[4])
                                         for e in own if e[3].startswith(('iomap:', 'filemap:', 'block:'))]
            row['async_timeline'] = [dict(at_ns=e[0], tid=e[2], event=e[3], detail=e[4])
                                     for e in io if e[3] == 'io_uring:io_uring_queue_async_work']
            row['caller_forks'] = sum(e[3] == 'sched:sched_process_fork' and e[2] == pid for e in own)
            row['fork_timeline'] = [dict(at_ns=e[0], detail=e[4]) for e in own if e[3] == 'sched:sched_process_fork']
            row['wake_timeline'] = [dict(at_ns=e[0], event=e[3], detail=e[4]) for e in es
                                    if e[3] in ('sched:sched_wakeup', 'sched:sched_switch')
                                    and any(':' + str(t) + ' [' in e[4] for t in (pid, issuer))]
            for role, tid in [('caller', pid), ('issuer', issuer)]:
                row.update({role + '_' + k: v for k, v in scheduling(es, tid).items()})
            rows.append(row)
        if not rows:
            raise ValueError('no measured windows')
        tail = sorted(rows, key=lambda r: r['latency_us'])[-max(1, len(rows) // 100):]
        metrics = [k for k in rows[0] if k.endswith('_us') or k in ('rings', 'sqes', 'cqes', 'enters', 'submitting_enters', 'cross_thread_cqes', 'caller_sleeps', 'issuer_sleeps', 'async_work', 'block_issues', 'paired_blocks', 'unmatched_block_issues', 'caller_forks')]
        def med(values):
            return {k: statistics.median(r[k] for r in values) for k in metrics}
        summary = dict(name=name, samples=len(rows), duplicate_perf_records=duplicate_records,
                       median=med(rows), slowest_one_percent_median=med(tail))
        counters = [k for k in rows[0] if k.startswith('window_') and k != 'window_devices']
        summary['window_io_totals'] = {k: sum(r[k] for r in rows) for k in counters}
        summary['window_io_samples'] = {k: sum(r[k] > 0 for r in rows) for k in counters}
        summaries.append(summary)
        (root / (name + '.timeline.json')).write_text(json.dumps(dict(summary=summary, samples=rows, tail=tail), indent=2) + '\n')
        print(json.dumps(summary), flush=True)
    (root / 'analysis.json').write_text(json.dumps(summaries, indent=2) + '\n')


def main():
    controls()
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('directory', type=Path, help='external tail artifact directory')
    a = p.parse_args()
    if a.directory.resolve().is_relative_to(Path(__file__).resolve().parents[1]):
        p.error('raw analysis belongs outside Git')
    analyze(a.directory)


def controls():
    # A blocked wait and a preemption are different causes of off-CPU time.
    es = [(0, 1, 1, 'sched:sched_switch', 'worker:1 [120] S ==> idle:0 [120]'),
          (10000, 0, 0, 'sched:sched_wakeup', 'worker:1 [120] CPU:001'),
          (12000, 0, 0, 'sched:sched_switch', 'idle:0 [120] R ==> worker:1 [120]'),
          (15000, 1, 1, 'sched:sched_switch', 'worker:1 [120] R+ ==> peer:2 [120]'),
          (18000, 2, 2, 'sched:sched_switch', 'peer:2 [120] S ==> worker:1 [120]')]
    if scheduling(es, 1) != dict(blocked_us=10, runnable_us=5, max_wake_to_run_us=3):
        raise ValueError('scheduler attribution control failed')
    try:
        scheduling([es[0], es[2]], 1)
    except ValueError:
        pass
    else:
        raise ValueError('missing scheduler receipt was accepted')
    events = [(0, 1, 1, 'block:block_rq_issue', '8,0 R 4096 () 100 + 8 0x0 [worker]'),
              (10000, 0, 0, 'block:block_rq_complete', '8,0 R () 100 + 8 0x0 [0]')]
    got = blocks(events, 1)
    if (got['paired_blocks'], got['max_block_wait_us']) != (1, 10):
        raise ValueError('block pairing control failed')
    if blocks(events, 2)['paired_blocks'] or blocks(events[:1], 1)['unmatched_block_issues'] != 1:
        raise ValueError('foreign or missing block completion was accepted')
    es = [(0, 1, 1, 'block:block_rq_issue', '8,0 W 8192 () 100 + 16 0x0 [reset]'),
          (20, 1, 1, 'block:block_rq_issue', '8,0 RS 4096 () 108 + 8 0x0 [worker]'),
          (30, 2, 2, 'block:block_rq_issue', '8,0 FWFS 4096 () 200 + 8 0x0 [jbd2]'),
          (40, 0, 0, 'block:block_rq_complete', '8,0 W () 100 + 16 0x0 [0]'),
          (45, 0, 0, 'block:block_rq_complete', '8,0 FWFS () 200 + 8 0x0 [0]'),
          (50, 0, 0, 'block:block_rq_complete', '8,0 RS () 108 + 8 0x0 [0]')]
    r = window_io(block_requests(es), 10, 60, 20, 1, 'R', 4096)
    if (r['window_other_writes'], r['window_carry_in_writes'], r['window_other_process_writes'],
        r['window_other_write_bytes'], r['window_flushes'], r['window_fua']) != (2, 1, 1, 12288, 1, 1):
        raise ValueError('reset carry-in or kernel flush/FUA was lost')
    clean = window_io(block_requests([es[1], es[-1]]), 10, 60, 20, 1, 'R', 4096)
    if clean['window_other_blocks'] or clean['window_unpaired']:
        raise ValueError('clean reset invented extra I/O')
    # Overlapping issue-to-completion intervals, not submission count or the
    # foreign write, determine this batch's depth. A completed request is gone.
    depth_events = [
        (10, 1, 2, 'block:block_rq_issue', '8,0 WS 4096 () 8 + 8 0x0 [worker]'),
        (20, 1, 3, 'block:block_rq_issue', '8,0 WS 4096 () 24 + 8 0x0 [worker]'),
        (30, 0, 0, 'block:block_rq_complete', '8,0 WS () 8 + 8 0x0 [0]'),
        (40, 1, 4, 'block:block_rq_issue', '8,0 WS 4096 () 40 + 8 0x0 [worker]'),
        (50, 0, 0, 'block:block_rq_complete', '8,0 WS () 24 + 8 0x0 [0]'),
        (60, 0, 0, 'block:block_rq_complete', '8,0 WS () 40 + 8 0x0 [0]')]
    depth = window_io(block_requests(depth_events), 0, 70, 10, 1, 'W', 12288)
    if [(r['sector'], r['bytes'], r['tid'], r['batch_depth_at_issue'])
        for r in depth['direct_io_timeline']] != [(8, 4096, 2, 1), (24, 4096, 3, 2), (40, 4096, 4, 2)]:
        raise ValueError('block geometry or per-batch issue depth was misattributed')
    try:
        window_io(block_requests(es), 10, 60, 20, 1, 'R', 2048)
    except ValueError:
        pass
    else:
        raise ValueError('extra direct bytes were silently accepted')


if __name__ == '__main__':
    main()
