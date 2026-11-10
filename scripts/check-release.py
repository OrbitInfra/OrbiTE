#!/usr/bin/env python3
"""Check source versions and release selection without contacting a registry (Python 3.11+)."""
import argparse
import json
import re
from pathlib import Path
import tomllib

ROOT = Path(__file__).resolve().parents[1]


def require(condition, message):
    if not condition:
        raise ValueError(message)


def versions(root=ROOT):
    manifest = tomllib.loads((root / "Cargo.toml").read_text())
    version = manifest["workspace"]["package"]["version"]
    match = re.fullmatch(r"(\d+\.\d+\.\d+)(?:-(alpha|beta|rc)\.(\d+))?", version)
    require(match is not None, "unsupported release version")
    python = match[1] + ({"alpha": "a", "beta": "b", "rc": "rc"}[match[2]] + match[3] if match[2] else "")
    binding = tomllib.loads((root / "crates/orbite-python/Cargo.toml").read_text())
    require(manifest["package"]["version"] == {"workspace": True}, "core must inherit the workspace version")
    require(binding["package"]["version"] == {"workspace": True}, "Python must inherit the workspace version")
    require(binding["dependencies"]["orbite"]["version"] == "=" + version, "Python exact core pin mismatch")
    locked = tomllib.loads((root / "Cargo.lock").read_text())["package"]
    for name in ("orbite", "orbite-python"):
        require([p["version"] for p in locked if p["name"] == name] == [version], f"{name} lock version mismatch")
    project = tomllib.loads((root / "pyproject.toml").read_text())["project"]
    require(project["name"] == "orbite" and "version" not in project and "version" in project["dynamic"], "Python version must come from Cargo")
    readme = (root / "README.md").read_text()
    require(re.search(r"cargo install orbite --version (\S+)", readme)[1] == version, "README Rust version mismatch")
    require(re.search(r"pip install orbite==(\S+)", readme)[1] == python, "README Python version mismatch")
    require(re.search(r"## (\S+) — preparation", (root / "docs/releases.md").read_text())[1] == version, "release notes version mismatch")
    for package in (manifest["package"], binding["package"]):
        require(package["license"] == "Apache-2.0", "license drift")
        require(package["repository"] == "https://github.com/OrbitInfra/OrbiTE", "repository drift")
    require((root / "LICENSE").read_text().lstrip().startswith("Apache License"), "missing Apache license")
    return version, python


def selection(version, ref, event, dry_run):
    tag = f"v{version}"
    if ref.startswith("refs/tags/"):
        require(ref == "refs/tags/" + tag, f"tag {ref} does not match {tag}")
    publish = event == "push" or (event == "workflow_dispatch" and not dry_run)
    if publish:
        require(ref == "refs/tags/" + tag, "publication requires the matching existing tag")
    return tag, publish


def main():
    p = argparse.ArgumentParser()
    p.add_argument("--ref", default="")
    p.add_argument("--event", default="pull_request", choices=["pull_request", "push", "workflow_dispatch"])
    p.add_argument("--dry-run", default="true", choices=["true", "false"])
    p.add_argument("--output", type=Path)
    args = p.parse_args()
    rust, python = versions()
    tag, publish = selection(rust, args.ref, args.event, args.dry_run == "true")
    result = dict(rust=rust, python=python, tag=tag, publish=str(publish).lower())
    print(json.dumps(result, indent=2))
    if args.output:
        with args.output.open("a") as out:
            out.writelines(f"{key}={value}\n" for key, value in result.items())


if __name__ == "__main__":
    main()
