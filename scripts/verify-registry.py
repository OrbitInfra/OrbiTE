#!/usr/bin/env python3
"""Read-only registry receipt verification. Never uploads, skips or retries a publish."""
import argparse
import hashlib
import importlib.util
import json
from pathlib import Path
import time
import urllib.error
import urllib.request

spec = importlib.util.spec_from_file_location("release", Path(__file__).with_name("check-release.py"))
release = importlib.util.module_from_spec(spec)
spec.loader.exec_module(release)


class ReceiptPending(ValueError):
    """An uploaded artifact is not visible in the registry response yet."""


def verify(registry, files, data):
    if registry == "pypi":
        recorded = {x["filename"]: x["digests"]["sha256"] for x in data["urls"]}
    else:
        recorded = {files[0].name: data["version"]["checksum"]}
    for path in files:
        if path.name in recorded:
            release.require(recorded[path.name] == hashlib.sha256(path.read_bytes()).hexdigest(), f"registry receipt different: {path.name}")
    missing = [path.name for path in files if path.name not in recorded]
    if missing:
        raise ReceiptPending(f"registry receipt missing: {', '.join(missing)}")


def wait_for_receipt(registry, files, request):
    # Uploads are never retried. Only read visibility may lag an accepted upload.
    deadline = time.monotonic() + 60
    pending = ReceiptPending("registry receipt visibility deadline expired")
    for attempt in range(13):
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            break
        try:
            with urllib.request.urlopen(request, timeout=min(10, remaining)) as response:
                verify(registry, files, json.load(response))
            return
        except urllib.error.HTTPError as error:
            if error.code != 404:
                raise
            pending = ReceiptPending("registry receipt is not visible (HTTP 404)")
        except ReceiptPending as error:
            pending = error
        remaining = deadline - time.monotonic()
        if attempt < 12 and remaining > 0:
            time.sleep(min(5, remaining))
    raise pending


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
    wait_for_receipt(args.registry, files, request)
    print(f"{args.registry}: verified {len(files)} published checksums")


if __name__ == "__main__":
    main()
