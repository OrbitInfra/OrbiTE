"""Check the shared CPU trace on Rust, an installed wheel or frozen native TENT."""

from __future__ import annotations

import argparse
from contextlib import suppress
import difflib
import hashlib
import json
import os
import platform
import re
import shlex
import signal
import subprocess
import sys
import tarfile
import time
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
CONTRACT = json.loads((REPO / "tests/reference/contract.json").read_text())


def digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def dependencies(linkage: str, build: Path) -> dict:
    result = {}
    for name, path in re.findall(r"^\s*(\S+)\s+=>\s+(\S+)", linkage, re.MULTILINE):
        if path == "not":
            raise RuntimeError("missing native dependency: " + name)
        resolved = Path(path).resolve()
        if name.startswith(
            ("libtent", "libtransfer_engine", "libmooncake", "libasio")
        ) and not resolved.is_relative_to(build):
            raise RuntimeError(
                "native dependency escaped isolated build: " + str(resolved)
            )
        result[name] = {"path": str(resolved), "sha256": digest(resolved)}
    return result


def observations(stdout: str) -> list[str]:
    return [line for line in stdout.splitlines() if line.startswith("OBS\t")]


def compare(expected: list[str], actual: list[str]) -> None:
    if expected != actual:
        difference = "\n".join(
            difflib.unified_diff(
                expected, actual, fromfile="expected", tofile="actual", lineterm=""
            )
        )
        raise ValueError("CPU trace mismatch:\n" + difference)


def negative_controls(expected: list[str]) -> list[str]:
    controls = []
    for name, kind, field, value in [
        ("corrupt-byte", "BYTES", 3, "00"),
        ("wrong-byte-count", "RESULT", 5, "999"),
        ("nonterminal-status", "RESULT", 3, "pending"),
        ("duplicate-message", "TAKE", 3, "999"),
    ]:
        changed = expected.copy()
        index = next(i for i, line in enumerate(changed) if line.split("\t")[1] == kind)
        fields = changed[index].split("\t")
        fields[field] = value
        if name == "corrupt-byte":
            original = expected[index].split("\t")[field]
            fields[field] = f"{int(original[:2], 16) ^ 1:02x}" + original[2:]
        elif name == "duplicate-message":
            fields[3] = str(int(expected[index].split("\t")[3]) + 1)
            fields[4] += "," + fields[4]
        changed[index] = "\t".join(fields)
        try:
            compare(expected, changed)
        except ValueError:
            controls.append(name)
        else:
            raise RuntimeError("comparator accepted " + name)
    # Keep the receipt labels but attribute each byte count to its sibling.
    swapped = expected.copy()
    label = next(row.split("\t")[2] for row in expected if row.startswith("OBS\tLIVE\t"))
    first, second = (
        next(i for i, row in enumerate(expected)
             if row.startswith(f"OBS\tRESULT\t{label}-{side}\t"))
        for side in ("first", "second")
    )
    a, a_bytes = expected[first].rsplit("\t", 1)
    b, b_bytes = expected[second].rsplit("\t", 1)
    swapped[first], swapped[second] = f"{a}\t{b_bytes}", f"{b}\t{a_bytes}"
    for name, changed in [
        ("swapped-batch-byte-counts", swapped),
        ("missing-cleanup", expected[:-1]),
        ("extra-observation", expected + [expected[-1]]),
    ]:
        try:
            compare(expected, changed)
        except ValueError:
            controls.append(name)
        else:
            raise RuntimeError("comparator accepted " + name)
    return controls


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument(
        "--rust-only",
        action="store_true",
        help="fixture gate only; never claims native comparison",
    )
    mode.add_argument(
        "--python-wheel",
        type=Path,
        help="install this wheel in a fresh virtualenv and check the shared fixture",
    )
    mode.add_argument(
        "--fetch-reference",
        action="store_true",
        help="fetch the frozen public reference and YLT archive into the artifact directory",
    )
    mode.add_argument(
        "--tent-source",
        type=Path,
        help="clean external checkout at the frozen commit, including pybind11",
    )
    parser.add_argument(
        "--ylt-archive",
        type=Path,
        help="the archive named in frozen FindYLT.cmake; SHA-256 is checked",
    )
    parser.add_argument(
        "--output", type=Path, required=True, help="new external artifact directory"
    )
    parser.add_argument("--jobs", type=int, default=4, choices=range(1, 17))
    args = parser.parse_args()
    output = args.output.resolve()
    if output.is_relative_to(REPO) or (
        args.tent_source and output.is_relative_to(args.tent_source.resolve())
    ):
        parser.error("output must be outside both source checkouts")
    if args.tent_source and not args.ylt_archive:
        parser.error("--tent-source requires --ylt-archive")
    if args.rust_only and args.ylt_archive:
        parser.error("--rust-only does not consume a YLT archive")
    if args.python_wheel and args.ylt_archive:
        parser.error("--python-wheel does not consume a YLT archive")
    output.mkdir(parents=True, exist_ok=False)
    manifest = {
        "schema_version": 1,
        "status": "running",
        "qualification": "P0.1 scoped CPU trace; independent acceptance pending",
        "profile": CONTRACT["profile"],
        "native_executed": False,
        "performance_claim": False,
        "commands": [],
        "platform": platform.platform(),
        "contract": CONTRACT,
    }
    manifest_path = output / "manifest.json"

    def save() -> None:
        manifest_path.write_text(json.dumps(manifest, indent=2) + "\n")

    def passed(status: str) -> int:
        manifest["status"] = status
        save()
        print(f"{status}: {manifest['observations']} observations; {manifest_path}")
        return 0

    # Do not inherit a consumer's native loader paths or Mooncake configuration.
    environment = {
        key: value
        for key, value in os.environ.items()
        if not key.startswith(("MC_", "TENT_", "MOONCAKE_"))
    }
    for key in ("LD_LIBRARY_PATH", "LD_PRELOAD"):
        environment.pop(key, None)

    def run(name: str, command: list[str], cwd: Path = REPO, timeout: int = 120) -> str:
        record = {"name": name, "command": command, "cwd": str(cwd)}
        manifest["commands"].append(record)
        started = time.monotonic()
        stdout = output / (name + ".stdout")
        stderr = output / (name + ".stderr")
        record.update(stdout=str(stdout), stderr=str(stderr))
        save()
        with stdout.open("w") as out, stderr.open("w") as err:
            process = subprocess.Popen(
                command,
                cwd=cwd,
                env=environment,
                stdout=out,
                stderr=err,
                start_new_session=True,
            )
            succeeded = False
            try:
                succeeded = process.wait(timeout=timeout) == 0
            except subprocess.TimeoutExpired:
                record["timed_out"] = True
            finally:
                if not succeeded:
                    # A failed controller can leave a stopped source behind,
                    # even when the controller has already been reaped.
                    with suppress(ProcessLookupError):
                        os.killpg(process.pid, signal.SIGKILL)
                    process.wait()
                record.update(exit_code=process.returncode,
                              elapsed_seconds=round(time.monotonic() - started, 3))
                save()
        if not succeeded:
            raise RuntimeError(f"{name} failed ({process.returncode}); see {stderr}")
        return stdout.read_text()

    def pending_read(profile: str, driver: list[str]) -> list[str]:
        receipts = run(
            f"{profile}-pending-read",
            [sys.executable, "-I", "-B", str(REPO / "tests/reference/pending_read.py"), *driver],
            cwd=output,
            timeout=30,
        ).splitlines()
        manifest.setdefault("pending_read", {})[profile] = receipts
        return receipts

    try:
        save()
        packaged_vcs = REPO / ".cargo_vcs_info.json"
        if packaged_vcs.is_file():
            manifest["source"] = {
                "revision": json.loads(packaged_vcs.read_text())["git"]["sha1"],
                "origin": "cargo-package",
                "dirty": None,
            }
        else:
            revision = run("source-revision", ["git", "rev-parse", "HEAD"]).strip()
            dirty = bool(run("source-status", ["git", "status", "--porcelain"]).strip())
            manifest["source"] = {"revision": revision, "origin": "git", "dirty": dirty}
        inputs = [
            Path(CONTRACT["trace"]),
            Path(CONTRACT["expected"]),
            Path("tests/reference/contract.json"),
            Path("tests/reference/run.py"),
            Path("tests/reference/pending_read.py"),
        ]
        if args.python_wheel:
            driver = Path("tests/reference/python_driver.py")
            inputs.append(driver)
        else:
            inputs.extend(
                [
                    Path("tests/reference/rust_driver.rs"),
                    Path("tests/reference/pending_read.rs"),
                    Path("tests/reference/tent_driver.cpp"),
                ]
            )
            if not args.rust_only:
                inputs.append(Path("tests/reference/tebench.py"))
        manifest["input_sha256"] = {str(path): digest(REPO / path) for path in inputs}
        expected = (REPO / CONTRACT["expected"]).read_text().splitlines()
        manifest["negative_controls"] = negative_controls(expected)
        trace = REPO / CONTRACT["trace"]
        if args.python_wheel:
            wheel = args.python_wheel.resolve()
            if wheel.suffix != ".whl" or not wheel.is_file():
                raise ValueError(f"expected an existing wheel: {wheel}")
            manifest["profile"] = "Installed CPython wheel, loopback TCP: two-engine trace and three-process stopped-source READs on one segment with independent peer progress"
            manifest["python_wheel"] = {"path": str(wheel), "sha256": digest(wheel)}
            venv = output / "venv"
            python = str(venv / "bin/python")
            run(
                "python-venv", [sys.executable, "-I", "-m", "venv", str(venv)], cwd=output
            )
            manifest["python"] = run("python-version", [python, "-VV"], cwd=output).strip()
            run(
                "python-install",
                [
                    python,
                    "-I",
                    "-m",
                    "pip",
                    "--isolated",
                    "--disable-pip-version-check",
                    "install",
                    "--no-index",
                    "--no-deps",
                    str(wheel),
                ],
                cwd=output,
            )
            actual = run(
                "python-trace",
                [python, "-I", "-B", str(REPO / driver), str(trace)],
                cwd=output,
                timeout=30,
            )
            compare(expected, observations(actual))
            manifest["observations"] = len(expected)
            pending_read("python", [python, "-I", "-B", str(REPO / driver)])
            return passed("passed-python-wheel-trace")

        manifest["rustc"] = run("rustc-version", ["rustc", "-Vv"]).strip()
        run(
            "rust-build",
            [
                "cargo",
                "build",
                "--locked",
                "--example",
                "cpu_trace",
                "--target-dir",
                str(output / "cargo"),
            ],
        )
        rust_binary = output / "cargo/debug/examples/cpu_trace"
        rust_output = run("rust-trace", [str(rust_binary), str(trace)], timeout=30)
        rust_records = observations(rust_output)
        compare(expected, rust_records)
        rust_pending = pending_read("rust", [str(rust_binary)])
        rust_linkage = run("rust-linkage", ["ldd", str(rust_binary)])
        if re.search(r"lib(?:tent|mooncake|asio)", rust_linkage):
            raise RuntimeError("Rust runner unexpectedly depends on a Mooncake runtime")
        manifest["rust_binary"] = {
            "path": str(rust_binary),
            "sha256": digest(rust_binary),
        }
        manifest["observations"] = len(expected)
        if not args.rust_only:
            if args.fetch_reference:
                source = output / "reference"
                run("reference-init", ["git", "init", str(source)], cwd=output)
                run(
                    "reference-fetch",
                    [
                        "git",
                        "fetch",
                        "--depth=1",
                        CONTRACT["tent_repository"],
                        CONTRACT["tent_revision"],
                    ],
                    cwd=source,
                    timeout=180,
                )
                run(
                    "reference-checkout",
                    ["git", "checkout", "--detach", "FETCH_HEAD"],
                    cwd=source,
                )
                run(
                    "reference-submodules",
                    [
                        "git",
                        "submodule",
                        "update",
                        "--init",
                        "--depth=1",
                        "extern/pybind11",
                    ],
                    cwd=source,
                    timeout=180,
                )
                archive = output / "yalantinglibs.tar.gz"
                url = (
                    "https://github.com/alibaba/yalantinglibs/archive/"
                    + CONTRACT["ylt_revision"]
                    + ".tar.gz"
                )
                run(
                    "ylt-download",
                    [
                        "curl",
                        "--fail",
                        "--location",
                        "--max-time",
                        "120",
                        "--output",
                        str(archive),
                        url,
                    ],
                    cwd=output,
                    timeout=130,
                )
            else:
                source = args.tent_source.resolve()
                archive = args.ylt_archive.resolve()
            native_revision = run(
                "tent-revision", ["git", "rev-parse", "HEAD"], cwd=source
            ).strip()
            if native_revision != CONTRACT["tent_revision"]:
                raise ValueError("native source is not the frozen TENT revision")
            if run(
                "tent-status",
                ["git", "status", "--porcelain", "--untracked-files=all"],
                cwd=source,
            ).strip():
                raise ValueError("native source checkout is modified")
            submodule_revision = run(
                "pybind11-revision",
                ["git", "rev-parse", "HEAD"],
                cwd=source / "extern/pybind11",
            ).strip()
            if submodule_revision != CONTRACT["pybind11_revision"]:
                raise ValueError("pybind11 revision differs from the frozen gitlink")
            if digest(archive) != CONTRACT["ylt_archive_sha256"]:
                raise ValueError("YLT archive SHA-256 mismatch")
            with tarfile.open(archive) as package:
                package.extractall(output / "deps", filter="data")
            ylt = output / ("deps/yalantinglibs-" + CONTRACT["ylt_revision"])
            build = output / "tent-build"
            flags = [
                f"-D{key}={value}" for key, value in CONTRACT["cmake_options"].items()
            ]
            flags.append("-DCMAKE_EXPORT_COMPILE_COMMANDS=ON")
            flags.append("-DFETCHCONTENT_SOURCE_DIR_YALANTINGLIBS=" + str(ylt))
            run(
                "tent-configure",
                [
                    "cmake",
                    "-S",
                    str(source),
                    "-B",
                    str(build),
                    *flags,
                ],
            )
            run(
                "tent-build",
                [
                    "cmake",
                    "--build",
                    str(build),
                    "--target",
                    "tent_shared",
                    "tebench",
                    "--parallel",
                    str(args.jobs),
                ],
                timeout=900,
            )
            library = build / "mooncake-transfer-engine/tent/src/libtent_shared.so"
            binary = output / "tent-driver"
            compiler = shlex.split(os.environ.get("CXX", "c++"))
            manifest["cxx"] = run("cxx-version", [*compiler, "--version"]).strip()
            run(
                "tent-driver-build",
                [
                    *compiler,
                    "-std=c++20",
                    "-O2",
                    "-Wall",
                    "-Wextra",
                    "-Werror",
                    "-I",
                    str(source / "mooncake-transfer-engine/tent/include"),
                    str(REPO / "tests/reference/tent_driver.cpp"),
                    str(library),
                    "-Wl,-rpath," + str(library.parent),
                    "-o",
                    str(binary),
                ],
            )
            linkage = run("tent-linkage", ["ldd", str(binary)])
            manifest["native_dependencies"] = dependencies(linkage, build)
            manifest["native_executed"] = True
            native_output = run("tent-trace", [str(binary), str(trace)], timeout=30)
            native_records = observations(native_output)
            compare(expected, native_records)
            compare(rust_records, native_records)
            compare(rust_pending, pending_read("tent", [str(binary)]))
            tebench = build / "mooncake-transfer-engine/benchmark/tebench"
            manifest["tebench"] = {
                "path": str(tebench),
                "sha256": digest(tebench),
                "dependencies": dependencies(
                    run("tebench-linkage", ["ldd", str(tebench)]), build
                ),
            }
            manifest["tebench"].update(
                json.loads(
                    run(
                        "tebench-bytes",
                        [
                            sys.executable,
                            "-I",
                            "-B",
                            str(REPO / "tests/reference/tebench.py"),
                            str(tebench),
                            str(binary),
                            str(output),
                        ],
                        cwd=output,
                        timeout=130,
                    )
                )
            )
            if run(
                "tent-final-status",
                ["git", "status", "--porcelain", "--untracked-files=all"],
                cwd=source,
            ).strip():
                raise ValueError("native build modified the frozen source checkout")
            manifest["native_binary"] = {
                "path": str(binary),
                "sha256": digest(binary),
                "library_sha256": digest(library),
            }
        return passed(
            "passed-cpu-trace"
            if manifest["native_executed"]
            else "passed-rust-fixture-only"
        )
    except (Exception, KeyboardInterrupt) as error:
        detail = str(error) or type(error).__name__
        manifest.update(status="failed", error=detail)
        save()
        print(detail, file=sys.stderr)
        return 1


if __name__ == "__main__":
    def interrupted(signum, _frame):
        raise InterruptedError(f"interrupted by {signal.Signals(signum).name}")

    signal.signal(signal.SIGTERM, interrupted)
    sys.exit(main())
