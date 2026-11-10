#!/usr/bin/env python3
"""Reject outputs/untracked paths in source archives and unexpected wheel payloads."""
import argparse
from pathlib import Path, PurePosixPath
import subprocess
import tarfile
import zipfile

ROOT = Path(__file__).resolve().parents[1]


def require(ok, message):
    if not ok:
        raise ValueError(message)


def check_names(names, tracked, wheel=False):
    require(len(names) == len(set(names)), "duplicate archive paths")
    for name in names:
        p = PurePosixPath(name)
        require(not p.is_absolute() and ".." not in p.parts, f"unsafe path: {name}")
        require(not any(x in p.parts for x in ("target", "__pycache__", ".git", "html", "logs", "traces", "run-output")), f"generated directory: {name}")
        require(p.suffix not in (".log", ".html", ".pyc", ".prof", ".pcap", ".crate", ".whl"), f"generated output: {name}")
        if wheel:
            allowed = name in ("orbite/__init__.py", "orbite/quickstart.py")
            allowed |= len(p.parts) == 2 and p.parts[0] == "orbite" and p.name.startswith("_native.") and p.name.endswith(".so")
            allowed |= p.parts[0].endswith(".dist-info") and (p.name in ("METADATA", "WHEEL", "RECORD", "LICENSE", "entry_points.txt", "sboms.json") or "licenses" in p.parts or "sboms" in p.parts)
            allowed |= p.parts[0] == "orbite.libs" and ".so" in p.name
        else:
            allowed = name in tracked or name in ("Cargo.toml.orig", ".cargo_vcs_info.json", "PKG-INFO")
        require(allowed, f"unexpected package path: {name}")
        if not wheel and p.suffix in (".json", ".tsv", ".trace", ".expected", ".bin"):
            fixtures = {".cargo_vcs_info.json", "tests/faults/matrix.tsv", "tests/reference/contract.json",
                        "tests/reference/wire/rpc.json", "tests/fixtures/cpu-v1.trace", "tests/fixtures/cpu-v1.expected",
                        "tests/reference/hp_tcp/ack.bin", "tests/reference/hp_tcp/read.bin",
                        "tests/reference/hp_tcp/write.bin", "tests/reference/mnnvl/describe.bin", "tests/reference/shm/describe.bin"}
            require(name in fixtures, f"unexpected captured data: {name}")
        require(p.suffix != ".trace" or name == "tests/fixtures/cpu-v1.trace", f"raw trace: {name}")
    require(any(PurePosixPath(n).name == "LICENSE" for n in names), "missing LICENSE")


def main():
    p = argparse.ArgumentParser()
    p.add_argument("artifacts", type=Path, nargs="*")
    p.add_argument("--cargo-list", type=Path)
    args = p.parse_args()
    tracked = set(subprocess.check_output(["git", "ls-files"], cwd=ROOT, text=True).splitlines())
    if args.cargo_list:
        check_names(args.cargo_list.read_text().splitlines(), tracked)
    for path in args.artifacts:
        if path.suffix == ".whl":
            with zipfile.ZipFile(path) as archive:
                names = [n for n in archive.namelist() if not n.endswith("/")]
            check_names(names, tracked, wheel=True)
        else:
            with tarfile.open(path) as archive:
                members = [m for m in archive.getmembers() if not m.isdir()]
                require(all(m.isfile() for m in members), "archive links are not permitted")
                require(all(not PurePosixPath(m.name).is_absolute() and ".." not in PurePosixPath(m.name).parts for m in members), "unsafe archive path")
                roots = {m.name.split("/")[0] for m in members}
                require(len(roots) == 1, "multiple archive roots")
                names = [m.name.split("/", 1)[1] for m in members]
            check_names(names, tracked)
        print(f"{path.name}: {len(names)} clean files")


if __name__ == "__main__":
    main()
