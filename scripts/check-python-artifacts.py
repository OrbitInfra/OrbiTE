"""Check package versions in locally built wheels/sdists; never publish anything."""
import argparse
from email.parser import Parser
import json
from pathlib import Path
import re
import subprocess
import tarfile
import zipfile


def require(condition, message):
    if not condition:
        raise ValueError(message)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("artifacts", nargs="+", type=Path)
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[1]
    metadata = json.loads(subprocess.check_output(
        ["cargo", "metadata", "--locked", "--no-deps", "--format-version", "1"],
        cwd=root, text=True,
    ))
    packages = {p["name"]: p for p in metadata["packages"]}
    rust = packages["orbite"]["version"]
    require(packages["orbite-python"]["version"] == rust, "workspace version drift")
    dependency = next(d for d in packages["orbite-python"]["dependencies"] if d["name"] == "orbite")
    require(dependency["req"] == "=" + rust, "Python's exact core dependency drifted")
    match = re.fullmatch(r"(\d+\.\d+\.\d+)(?:-(alpha|beta|rc)\.(\d+))?", rust)
    require(match is not None, f"unhandled release version: {rust}")
    pep440 = match[1] + ({"alpha": "a", "beta": "b", "rc": "rc"}[match[2]] + match[3] if match[2] else "")
    for path in args.artifacts:
        if path.suffix == ".whl":
            with zipfile.ZipFile(path) as archive:
                names = [n for n in archive.namelist() if n.endswith(".dist-info/METADATA")]
                require(len(names) == 1, f"ambiguous wheel metadata: {path}")
                data = archive.read(names[0])
            require(path.name.startswith(f"orbite-{pep440}-"), f"wrong wheel filename: {path}")
        else:
            require(path.name == f"orbite-{pep440}.tar.gz", f"wrong sdist filename: {path}")
            with tarfile.open(path) as archive:
                member = archive.extractfile(f"orbite-{pep440}/PKG-INFO")
                require(member is not None, f"missing source metadata: {path}")
                data = member.read()
        info = Parser().parsestr(data.decode("utf-8"))
        require(info["Name"] == "orbite", f"wrong package name: {path}")
        require(info["Version"] == pep440, f"wrong package version: {path}: {info['Version']} != {pep440}")
        print(f"{path.name}: Rust {rust}, Python {pep440}")


if __name__ == "__main__":
    main()
