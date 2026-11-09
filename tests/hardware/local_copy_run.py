"""Compare packaged OrbiTE and frozen OrbitKV on one exclusively allocated CUDA GPU."""
import argparse
import fcntl
import hashlib
import json
import math
import os
from pathlib import Path
import statistics
import subprocess

ORBITKV = '93541a213184cd7a45a9ee92cf0d9ed41870518a'
CASES = ((1, 4096), (16, 4096), (257, 31), (1024, 1024))
SAMPLES = 30
WIDTHS = tuple(1 << i for i in range(12, 22))
COUNTS = tuple(1 << i for i in range(14))
MODES = (('dma', 0), ('batch', 0), *(('kernel', cap) for cap in (0, 1, 4, 16, 64)))


def calibration_rows(text, gpu, count, width, mode, cap, interference):
    rows = [json.loads(line) for line in text.splitlines()]
    if len(rows) != 2 or {r['direction'] for r in rows} != {'h2d', 'd2h'}:
        raise ValueError('incomplete calibration directions')
    for r in rows:
        for key, value in {'case': 'calibration', 'cuda_device_uuid': gpu, 'count': count,
                           'width': width, 'bytes': count * width, 'backend': mode,
                           'cta_limit': cap, 'interference': interference,
                           'samples': SAMPLES, 'warmup': 5,
                           'byte_source_guard_oracle': 'passed'}.items():
            if r[key] != value:
                raise ValueError(f'calibration mismatch: {key}')
        expected_intent, expected_priority = ('ForegroundGet', 'High') if r['direction'] == 'h2d' else ('Checkpoint', 'Low')
        if (r['intent'], r['priority']) != (expected_intent, expected_priority):
            raise ValueError('submission metadata did not reach the executing class')
        grid = r['kernel_ctas']
        if not isinstance(grid, int) or (mode == 'kernel' and not 1 <= grid <= (cap or 65535)) or (mode != 'kernel' and grid != 0):
            raise ValueError('invalid kernel grid evidence')
        if r['dma_batch_calls'] != (SAMPLES + 5 if mode == 'batch' and count > 1 else 0):
            raise ValueError('native batch DMA evidence mismatch')
        for key in ('copy_ms', 'compute_alone_ms', 'compute_with_copy_ms'):
            values = r[key]
            expected = SAMPLES if key == 'copy_ms' or interference else 0
            if len(values) != expected or not all(math.isfinite(v) and v > 0 for v in values):
                raise ValueError(f'invalid {key} samples')
        r['copy_gib_s'] = r['bytes'] / (1 << 30) / (statistics.median(r['copy_ms']) / 1000)
        r['compute_slowdown'] = (statistics.median(r['compute_with_copy_ms']) /
                                 statistics.median(r['compute_alone_ms'])) if interference else None
    return rows


def proposals(rows):
    """Measured candidates, not defaults: win versus both DMAs at <=5% probe slowdown."""
    result = []
    for direction in ('h2d', 'd2h'):
        for width in WIDTHS:
            winners = []
            for count in COUNTS:
                group = [r for r in rows if (r['direction'], r['width'], r['count'], r['interference']) == (direction, width, count, True)]
                dma = [r['copy_gib_s'] for r in group if r['backend'] != 'kernel']
                if len(dma) != 2:
                    continue
                eligible = [r for r in group if r['backend'] == 'kernel' and
                            r['compute_slowdown'] <= 1.05 and r['copy_gib_s'] > max(dma)]
                if eligible:
                    winners.append(max(eligible, key=lambda r: r['copy_gib_s']))
            first = winners[0] if winners else None
            background = next((r for r in winners if r['count'] > first['count']), None) if first and direction == 'd2h' else None
            result.append({'direction': direction, 'width': width,
                           'kernel_threshold_candidate': first['count'] if first else None,
                           'kernel_threshold_background_candidate': background['count'] if background else None,
                           'kernel_cta_limit_candidate': (first['cta_limit'] or None) if first else None,
                           'criterion': 'copy faster than both DMA modes; fixed compute slowdown <= 1.05; confirm across sizes and GPU classes before choosing defaults'})
    return result


def calibrate(args, manifest, binary, env, run, evidence):
    if not args.interference:
        raise ValueError('calibration requires --interference for the compute trade-off')
    with args.gpu_lock.open('r+') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        inventory = run('inventory', ['nvidia-smi', '-i', args.gpu, '--query-gpu=uuid,name,driver_version,pci.bus_id', '--format=csv,noheader'])
        if inventory.split(',')[0].strip() != args.gpu:
            raise ValueError('physical GPU identity mismatch')
        if args.platform == 'pcie-hopper' and not any(name in inventory for name in ('H100', 'H20')):
            raise ValueError('PCIe Hopper calibration requires H100/H20')
        if args.platform == 'grace-c2c' and not any(name in inventory for name in ('GH200', 'GB200')):
            raise ValueError('Grace calibration requires GH200/GB200')
        attestation = json.loads(args.topology_evidence.read_text())
        if (attestation.get('cuda_device_uuid'), attestation.get('platform'), attestation.get('verified_link')) != (args.gpu, args.platform, 'pcie' if args.platform == 'pcie-hopper' else 'c2c'):
            raise ValueError('operator topology attestation must identify this GPU and link')
        topology = run('topology', ['nvidia-smi', 'topo', '-m'])
        cpu = run('cpu', ['lscpu'])
        evidence.update(platform=args.platform, topology_attestation=attestation, topology=topology, cpu=cpu,
                        inventory=inventory, scope='per-GPU calibration; fixed ALU probe is not model ITL',
                        sizes=WIDTHS, counts=COUNTS, modes=MODES, samples=SAMPLES, warmup=5)
        rows = []
        for width in WIDTHS:
            for count in COUNTS:
                for mode, cap in MODES:
                    for interference in (False, True):
                        name = f'calibrate-{width}-{count}-{mode}-{cap}-{int(interference)}'
                        raw = run(name, [binary, '--calibrate', str(count), str(width), str(SAMPLES), mode, str(cap), str(int(interference))], timeout=7200)
                        rows.extend(calibration_rows(raw, args.gpu, count, width, mode, cap, interference))
        evidence.update(rows=rows, proposals=proposals(rows), status='passed',
                        qualification='only this named installed GPU; defaults unchanged')


def digest(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def checked_rows(text, gpu, blocks, width):
    rows = [json.loads(line) for line in text.splitlines()]
    if len(rows) != 2 or {r['direction'] for r in rows} != {'h2d', 'd2h'}:
        raise ValueError('expected one complete result per direction')
    medians = {}
    for row in rows:
        for key, expected in {'case': 'custom-fragments', 'cuda_device_uuid': gpu,
                              'descriptors': blocks, 'sizes': [width] * blocks,
                              'samples': SAMPLES, 'warmup': 5, 'bytes': blocks * width,
                              'byte_source_guard_oracle': 'passed'}.items():
            if row[key] != expected:
                raise ValueError(f'wrong {key} for {row["direction"]}')
        for backend in ('dma', 'kernel'):
            times = row[f'{backend}_submit_to_drain_ms']
            if len(times) != SAMPLES or not all(math.isfinite(t) and t > 0 for t in times):
                raise ValueError('incomplete or invalid submit-to-drain samples')
            medians[f'{row["direction"]}_{backend}_ms'] = statistics.median(times)
    return medians


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('artifact', 'gpu-lock', 'output'):
        parser.add_argument('--' + name, type=Path, required=True)
    parser.add_argument('--orbitkv', type=Path)
    parser.add_argument('--calibrate', action='store_true')
    parser.add_argument('--interference', action='store_true')
    parser.add_argument('--platform', choices=('pcie-hopper', 'grace-c2c'))
    parser.add_argument('--topology-evidence', type=Path, help='operator-verified PCIe or Grace C2C topology report')
    parser.add_argument('--gpu', required=True, help='assigned physical GPU UUID')
    args = parser.parse_args()
    if args.calibrate and (not args.platform or not args.topology_evidence):
        parser.error('--calibrate requires --platform and --topology-evidence')
    if not args.calibrate and not args.orbitkv:
        parser.error('the matched comparison requires --orbitkv')
    output, artifact = (p.resolve() for p in (args.output, args.artifact))
    checkout = args.orbitkv.resolve() if args.orbitkv else None
    if (checkout and output.is_relative_to(checkout)) or output.is_relative_to(artifact):
        raise ValueError('use a fresh external evidence directory')
    output.mkdir(parents=True, exist_ok=False)
    manifest = json.loads((artifact / 'manifest.json').read_text())
    binary = Path(manifest['binary'])
    driver = Path(__file__).resolve()
    if (manifest['status'] != 'passed' or manifest['dirty'] or manifest['consumer'] != 'local-copy'
            or manifest['protocol'] != 'orbite-installed-local-copy'
            or digest(binary) != manifest['binary_sha256']
            or digest(Path(manifest['archive'])) != manifest['archive_sha256']
            or digest(driver) != manifest['fixture_files_sha256']['tests/hardware/local_copy_run.py']):
        raise ValueError('packaged public consumer or driver identity mismatch')
    env = {**os.environ, 'CUDA_VISIBLE_DEVICES': args.gpu}
    evidence = {'status': 'running', 'gpu': args.gpu, 'orbite': manifest['source_commit'],
                'orbitkv': ORBITKV, 'artifact_sha256': manifest['archive_sha256'],
                'scope': 'CPU wall time from submission entry through physical completion on the default stream',
                'cases': []}

    def run(name, command, work=output, timeout=180):
        with (output / f'{name}.stdout').open('x') as stdout, (output / f'{name}.stderr').open('x') as stderr:
            subprocess.run(command, cwd=work, env=env, stdout=stdout, stderr=stderr, timeout=timeout, check=True)
        return (output / f'{name}.stdout').read_text()

    try:
        if run('protocol', [binary, '--protocol']).strip() != manifest['protocol']:
            raise ValueError('installed protocol mismatch')
        if args.calibrate:
            calibrate(args, manifest, binary, env, run, evidence)
            return
        # Build in a private copy. No command modifies the supplied OrbitKV checkout.
        source = output / 'orbitkv-src'
        run('clone', ['git', 'clone', '--no-checkout', '--no-hardlinks', checkout, source])
        run('checkout', ['git', 'checkout', '--detach', ORBITKV], source)
        run('mooncake', ['git', 'submodule', 'update', '--init', '--recursive', 'third-party/mooncake'], source, 1800)
        env['CARGO_TARGET_DIR'] = str(output / 'orbitkv-target')
        build = run('build-oracle', ['cargo', 'bench', '--locked', '--no-run', '--message-format=json',
                                     '-p', 'orbitkv-core', '--bench', 'native_kernel_copy',
                                     '--no-default-features', '--features', 'cuda-13,mooncake'], source, 3600)
        executables = [m['executable'] for m in map(json.loads, build.splitlines()) if
                       m.get('reason') == 'compiler-artifact' and m['target']['name'] == 'native_kernel_copy'
                       and m.get('executable')]
        if len(executables) != 1:
            raise ValueError('expected one frozen native_kernel_copy executable')
        oracle = Path(executables[0])
        evidence['orbitkv_binary_sha256'] = digest(oracle)
        # The operator supplies the physical GPU's shared reservation lock.
        with args.gpu_lock.open('r+') as lock:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            inventory = run('inventory', ['nvidia-smi', '-i', args.gpu, '--query-gpu=uuid,name,driver_version,pci.bus_id',
                                          '--format=csv,noheader'])
            if inventory.split(',')[0].strip() != args.gpu:
                raise ValueError('GPU inventory mismatch')
            for index, (blocks, width) in enumerate(CASES):
                baseline = output / f'orbitkv-{blocks}.jsonl'
                pair = {}
                for name in (('orbitkv', 'orbite') if index % 2 == 0 else ('orbite', 'orbitkv')):
                    if name == 'orbitkv':
                        run(f'oracle-{blocks}', [oracle, '--output', baseline, '--samples', str(SAMPLES),
                                                 '--blocks', str(blocks), '--fragment-bytes', str(width)])
                        data = baseline.read_text()
                    else:
                        data = run(f'orbite-{blocks}', [binary, str(blocks), str(width), str(SAMPLES)])
                    pair[name] = checked_rows(data, args.gpu, blocks, width)
                evidence['cases'].append({'blocks': blocks, 'fragment_bytes': width, 'medians': pair})
        evidence['status'] = 'passed'
    finally:
        (output / 'report.json').write_text(json.dumps(evidence | {'status': 'passed' if evidence['status'] == 'passed' else 'failed'},
                                                       indent=2) + '\n')


if __name__ == '__main__':
    main()
