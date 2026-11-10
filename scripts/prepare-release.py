#!/usr/bin/env python3
"""Select tested stable artifacts (never both wheel profiles); write their checksums."""
import argparse
import hashlib
import importlib.util
from pathlib import Path
import re
import shutil
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("release", ROOT / "scripts/check-release.py")
release = importlib.util.module_from_spec(spec)
spec.loader.exec_module(release)


def release_notes(text, version):
    notes = text.split("## Maintainer publication procedure", 1)[0]
    # A Release body has no docs/ base directory. Link to the approved tag.
    return re.sub(r"\]\(([^():\s]+\.md(?:#[^()\s]+)?)\)",
                  lambda m: f"](https://github.com/OrbitInfra/OrbiTE/blob/v{version}/docs/{m[1]})",
                  notes)


def main():
    p = argparse.ArgumentParser()
    p.add_argument("input", type=Path)
    p.add_argument("output", type=Path)
    args = p.parse_args()
    rust, python = release.versions()
    files = []
    for runner, arch in (("ubuntu-24.04", "x86_64"), ("ubuntu-24.04-arm", "aarch64")):
        for minor in range(10, 15):
            source = args.input / f"orbite-{runner}-3.{minor}-stable" / "wheel-combined"
            wheels = list(source.glob(f"orbite-{python}-cp3{minor}-cp3{minor}-manylinux_2_28_{arch}.whl"))
            release.require(len(wheels) == 1, f"missing/ambiguous wheel: {source}")
            files += wheels
    sdists = list((args.input / "orbite-ubuntu-24.04-3.13-stable" / "sdist").glob(f"orbite-{python}.tar.gz"))
    release.require(len(sdists) == 1, "missing/ambiguous sdist")
    crate = args.input / "crate-stable" / f"orbite-{rust}.crate"
    release.require(crate.is_file(), "missing tested crate")
    files += sdists + [crate]
    subprocess.run([sys.executable, str(ROOT / "scripts/check-python-artifacts.py"), *map(str, files[:-1])], check=True)
    subprocess.run([sys.executable, str(ROOT / "scripts/check-package-contents.py"), *map(str, files)], check=True)
    args.output.mkdir(parents=True, exist_ok=False)
    hashes = []
    for path in files:
        shutil.copy2(path, args.output / path.name)
        hashes.append(f"{hashlib.sha256(path.read_bytes()).hexdigest()}  {path.name}\n")
    (args.output / "SHA256SUMS").write_text("".join(sorted(hashes)))
    notes = release_notes((ROOT / "docs/releases.md").read_text(), rust)
    (args.output / "RELEASE-NOTES.md").write_text(notes)
    print(f"{len(files)} checked release artifacts; CPU and MSRV wheels remain review artifacts")


if __name__ == "__main__":
    main()
