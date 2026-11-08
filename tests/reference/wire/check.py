"""Build the frozen TENT client solely to verify native protocol rejection."""
import argparse
import json
import os
from pathlib import Path
import shlex
import subprocess
HERE = Path(__file__).resolve().parent
def build(source, build_dir, output):
    unit = next(
        u
        for u in json.loads((build_dir / "compile_commands.json").read_text())
        if u["file"].endswith("/tent/src/rpc/rpc.cpp")
    )
    args = shlex.split(unit["command"]) if "command" in unit else unit["arguments"].copy()
    del args[args.index("-o") : args.index("-o") + 2]
    args.remove("-c")
    args.remove(unit["file"])
    lib = build_dir / "mooncake-transfer-engine/tent/src/libtent_shared.so"
    binary = output / "wire-peer"
    subprocess.run(
        [
            *args,
            str(HERE / "native.cpp"),
            str(lib),
            str(build_dir / "mooncake-common/libasio.so"),
            str(lib.parent / "metrics/libtent_metrics.so"),
            "-Wl,-rpath,"
            + ":".join(map(str, (lib.parent, lib.parent / "metrics", build_dir / "mooncake-common"))),
            "-lglog",
            "-o",
            str(binary),
        ],
        cwd=unit["directory"],
        check=True,
        timeout=240,
    )
    return binary


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--source', type=Path, required=True)
    parser.add_argument('--build', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=True)
    binary = build(args.source.resolve(), args.build.resolve(), args.output.resolve())
    subprocess.run(['cargo', 'test', '--locked', '--lib',
        'runtime::control_plane::tests::native_boundary::frozen_tent_client_is_rejected_without_admission_or_payload',
        '--', '--exact', '--ignored'], check=True,
        env=dict(os.environ, ORBITE_TEST_FROZEN_CLIENT=str(binary)))
    (args.output / 'native-boundary.json').write_text(json.dumps({'frozen_client':str(binary), 'rejected':['describe','write','read']})+'\n')

if __name__ == '__main__':
    main()
