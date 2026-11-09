"""Installed two-host GPUDirect route, byte and retirement gate; never auto-run."""
import argparse
import hashlib
import json
import os
import time
from pathlib import Path
import selectors
import shlex
import subprocess


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main(a):
    out = a.output.resolve()
    out.mkdir(parents=True, exist_ok=False)
    artifact = a.artifact.resolve(strict=True)
    manifest = json.loads((artifact / 'manifest.json').read_text())
    binary = artifact / 'installed/bin/orbite-gdr-dmabuf'
    if (manifest['consumer'] != 'gdr-dmabuf' or manifest['status'] != 'passed'
            or manifest['dirty'] or digest(binary) != manifest['binary_sha256']
            or digest(Path(__file__)) != manifest['fixture_files_sha256']['tests/hardware/gdr_dmabuf_run.py']):
        raise ValueError('installed artifact identity mismatch')
    commands, processes, files = [], [], []
    buffered = {}
    remote_binary = a.source_binary or str(binary)

    def command(host, words):
        return ['ssh', '-oBatchMode=yes', host, shlex.join(words)] if host else words

    def inventory(label, host):
        script = 'uname -a; cat /proc/sys/kernel/random/boot_id; rdma link; ibv_devinfo; nvidia-smi -q; cat /proc/modules'
        result = subprocess.run(command(host, ['sh', '-ec', script]), check=True, capture_output=True, text=True, timeout=45)
        (out / (label + '.inventory')).write_text(result.stdout + result.stderr)
        return subprocess.check_output(command(host, ['cat', '/proc/sys/kernel/random/boot_id']), text=True, timeout=10).strip()

    def spawn(name, role, peers=(), bind=None):
        remote = role == 'source'
        words = ['env', 'MLX5_SCATTER_TO_CQE=0', 'CUDA_VISIBLE_DEVICES=' + (a.source_gpu if remote else a.gpu),
                 remote_binary if remote else str(binary), role, a.route,
                 bind or (a.source_bind if remote else a.bind),
                 a.source_device if remote else a.device, str(a.gid_index), *peers]
        argv = command(a.source_host if remote else None, words)
        commands.append(argv)
        err = (out / (name + '.stderr')).open('w')
        files.append(err)
        p = subprocess.Popen(argv, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=err, bufsize=0)
        processes.append(p)
        return p, name

    def line(child, prefix):
        p, name = child
        deadline = time.monotonic() + 180
        while True:
            pending = buffered.get(name, b'')
            while b'\n' not in pending:
                left = deadline - time.monotonic()
                with selectors.DefaultSelector() as poll:
                    poll.register(p.stdout, selectors.EVENT_READ)
                    if left <= 0 or not poll.select(left):
                        raise TimeoutError(name)
                    data = os.read(p.stdout.fileno(), 4096)
                if not data or len(pending) + len(data) > 8192:
                    raise RuntimeError(name + ': EOF or oversized response')
                pending += data
            value, pending = pending.split(b'\n', 1)
            buffered[name] = pending
            value = value.decode().strip()
            with (out / (name + '.stdout')).open('a') as log:
                log.write(value + '\n')
            if value.startswith(prefix):
                return value.split()[1:]
            if not value.startswith('ROUTE '):
                raise RuntimeError(f'{name}: expected {prefix}, got {value!r}')

    def send(child, text):
        child[0].stdin.write((text + '\n').encode())
        child[0].stdin.flush()

    def finish(child):
        send(child, 'VERIFY')
        line(child, 'PASS source')
        if child[0].wait(timeout=90):
            raise RuntimeError(child[1] + ' failed')

    report = {'source_commit': manifest['source_commit'], 'package_sha256': manifest['archive_sha256'],
              'profile': a.route, 'commands': commands, 'status': 'failed', 'hardware_run': True}
    try:
        remote_hash = subprocess.check_output(command(a.source_host, ['sha256sum', remote_binary]), text=True, timeout=20).split()[0]
        if remote_hash != manifest['binary_sha256']:
            raise ValueError('source binary differs from requester artifact')
        if inventory('source-before', a.source_host) == inventory('requester-before', None):
            raise ValueError('this profile requires distinct hosts')
        source = spawn('source-old', 'source')
        endpoint, epoch = line(source, 'READY ')
        requester = spawn('requester', 'requester', (endpoint, epoch))
        line(requester, 'RETIRED')
        finish(source)
        fresh = spawn('source-new', 'source', bind=endpoint)
        new_endpoint, new_epoch = line(fresh, 'READY ')
        if new_endpoint != endpoint or new_epoch == epoch:
            raise ValueError('restart endpoint/epoch mismatch')
        send(requester, f'NEW {new_epoch}')
        line(requester, 'PASS requester')
        if requester[0].wait(timeout=90):
            raise RuntimeError('requester failed')
        finish(fresh)
        inventory('source-after', a.source_host)
        inventory('requester-after', None)
        report['status'] = 'passed'
    finally:
        for p in processes:
            if p.poll() is None:
                p.terminate()
            try:
                p.wait(timeout=5)
            except subprocess.TimeoutExpired:
                p.kill()
                p.wait(timeout=5)
        for f in files:
            f.close()
        (out / 'report.json').write_text(json.dumps(report, indent=2) + '\n')


if __name__ == '__main__':
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--artifact', type=Path, required=True)
    p.add_argument('--output', type=Path, required=True)
    p.add_argument('--route', choices=('dmabuf', 'peermem'), required=True)
    p.add_argument('--source-host', required=True)
    p.add_argument('--source-binary')
    p.add_argument('--source-bind', required=True)
    p.add_argument('--bind', required=True)
    p.add_argument('--device', required=True)
    p.add_argument('--source-device', required=True)
    p.add_argument('--gid-index', type=int, default=0)
    p.add_argument('--gpu', default='0')
    p.add_argument('--source-gpu', default='0')
    main(p.parse_args())
