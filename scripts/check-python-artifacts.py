"""Check package versions in locally built wheels/sdists; never publish anything."""
import argparse
from email.parser import Parser
import json
from pathlib import Path
import re
import subprocess
import tarfile
import zipfile


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
    assert packages["orbite-python"]["version"] == rust, "workspace version drift"
    dependency = next(d for d in packages["orbite-python"]["dependencies"] if d["name"] == "orbite")
    assert dependency["req"] == "^" + rust, "Python's core dependency minimum drifted"
    match = re.fullmatch(r"(\d+\.\d+\.\d+)(?:-(alpha|beta|rc)\.(\d+))?", rust)
    assert match, f"unhandled release version: {rust}"
    pep440 = match[1] + ({"alpha": "a", "beta": "b", "rc": "rc"}[match[2]] + match[3] if match[2] else "")
    for path in args.artifacts:
        if path.suffix == ".whl":
            with zipfile.ZipFile(path) as archive:
                names = [n for n in archive.namelist() if n.endswith(".dist-info/METADATA")]
                assert len(names) == 1, f"ambiguous wheel metadata: {path}"
                data = archive.read(names[0])
            assert path.name.startswith(f"orbite-{pep440}-"), path
        else:
            assert path.name == f"orbite-{pep440}.tar.gz", path
            with tarfile.open(path) as archive:
                member = archive.extractfile(f"orbite-{pep440}/PKG-INFO")
                assert member is not None, f"missing source metadata: {path}"
                data = member.read()
        info = Parser().parsestr(data.decode("utf-8"))
        assert info["Name"] == "orbite", path
        assert info["Version"] == pep440, (path, info["Version"], pep440)
        print(f"{path.name}: Rust {rust}, Python {pep440}")


if __name__ == "__main__":
    main()
