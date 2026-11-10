#!/usr/bin/env python3
"""Read-only registry receipt verification. Never uploads, skips or retries a publish."""
import argparse
import hashlib
import importlib.util
import json
from pathlib import Path
import urllib.request

spec = importlib.util.spec_from_file_location("release", Path(__file__).with_name("check-release.py"))
release = importlib.util.module_from_spec(spec)
spec.loader.exec_module(release)


def verify(registry, files, data):
    if registry == "pypi":
        recorded = {x["filename"]: x["digests"]["sha256"] for x in data["urls"]}
    else:
        recorded = {files[0].name: data["version"]["checksum"]}
    for path in files:
        release.require(recorded.get(path.name) == hashlib.sha256(path.read_bytes()).hexdigest(), f"registry receipt missing or different: {path.name}")


def main():
    p = argparse.ArgumentParser()
    p.add_argument("registry", choices=["pypi", "crates-io"])
    p.add_argument("artifacts", type=Path)
    args = p.parse_args()
    rust, python = release.versions()
    if args.registry == "pypi":
        url = f"https://pypi.org/pypi/orbite/{python}/json"
        files = sorted(args.artifacts.glob("*.whl")) + sorted(args.artifacts.glob("*.tar.gz"))
        release.require(len(files) == 11, "expected ten wheels and one sdist")
    else:
        url = f"https://crates.io/api/v1/crates/orbite/{rust}"
        files = list(args.artifacts.glob(f"orbite-{rust}.crate"))
        release.require(len(files) == 1, "expected one crate")
    request = urllib.request.Request(url, headers={"User-Agent": "OrbiTE release verification (https://github.com/OrbitInfra/OrbiTE)"})
    with urllib.request.urlopen(request, timeout=30) as response:
        verify(args.registry, files, json.load(response))
    print(f"{args.registry}: verified {len(files)} published checksums")


if __name__ == "__main__":
    main()
