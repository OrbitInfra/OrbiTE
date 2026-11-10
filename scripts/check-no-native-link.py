#!/usr/bin/env python3
"""Reject native SDK imports (and auditwheel-vendored providers) in release ELFs."""
import re
import subprocess
import sys
import tempfile
from pathlib import Path
from zipfile import ZipFile

NATIVE = re.compile(r"lib(?:ibverbs|rdmacm|mlx[45]|cuda|cufile)[-.]")


def check(path):
    dynamic = subprocess.check_output(["readelf", "-dW", str(path)], text=True)
    symbols = subprocess.check_output(["readelf", "--dyn-syms", "-W", str(path)], text=True)
    for line in dynamic.splitlines():
        if "(NEEDED)" in line and NATIVE.search(line):
            raise SystemExit(f"{path}: native runtime link: {line.strip()}")
    for line in symbols.splitlines():
        if " UND " in line and re.search(r"\b_?ibv_", line):
            raise SystemExit(f"{path}: unresolved verbs symbol: {line.strip()}")
    print(f"{path.name}: no native SDK NEEDED or undefined ibv_* exports")


def main():
    if len(sys.argv) < 2:
        raise SystemExit("usage: check-no-native-link.py ELF_OR_WHEEL ...")
    for name in sys.argv[1:]:
        path = Path(name)
        if path.suffix != ".whl":
            check(path)
            continue
        with ZipFile(path) as wheel, tempfile.TemporaryDirectory() as directory:
            libraries = [n for n in wheel.namelist() if ".so" in Path(n).name]
            if not libraries:
                raise SystemExit(f"{path}: no native extension")
            for index, member in enumerate(libraries):
                if NATIVE.search(Path(member).name):
                    raise SystemExit(f"{path}: bundled native SDK: {member}")
                elf = Path(directory) / f"{index}-{Path(member).name}"
                elf.write_bytes(wheel.read(member))
                check(elf)


if __name__ == "__main__":
    main()
