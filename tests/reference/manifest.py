"""Verify a CPU oracle manifest and its adjacent logs, including after relocation."""

import argparse
import hashlib
import json
import math
import re
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
# Direct invocation with python -I omits the script directory.
if __name__ == "__main__":
    sys.path.insert(0, str(HERE))
from tebench import check_workload_row, initiator_command

ROOT = HERE.parents[1]
SCHEMA = json.loads((HERE / "manifest.schema.json").read_text())
CONTRACT = json.loads((HERE / "contract.json").read_text())
PROFILES = {rule["properties"]["status"]["const"]: rule for rule in SCHEMA["oneOf"]}
READ_CONTROLS = {"pending_read": [], "interrupted_read": ["--interrupted"], "restart_read": ["--restart"]}

DRIVER_COMMANDS = {
    "rust": ("rustc-version", "rust-build", "rust-linkage"),
    "tent": (
        "tent-revision",
        "tent-status",
        "pybind11-revision",
        "tent-configure",
        "tent-build",
        "cxx-version",
        "tent-driver-build",
        "tent-linkage",
        "tebench-linkage",
        "tebench-bytes",
        "tent-final-status",
    ),
    "python": ("python-version", "python-venv", "python-install"),
}


def require(condition, message):
    if not condition:
        raise ValueError(message)


def load_manifest(path):
    def unique_fields(pairs):
        obj = {}
        for key, value in pairs:
            require(key not in obj, f"duplicate JSON field: {key}")
            obj[key] = value
        return obj

    return json.loads(path.read_text(), object_pairs_hook=unique_fields)


def required_keys(obj, keys, label):
    require(isinstance(obj, dict), f"{label}: expected an object")
    for key in keys:
        require(key in obj, f"{label}: missing {key}")


def sha256(value):
    return isinstance(value, str) and re.fullmatch(r"[0-9a-f]{64}", value) is not None


def check_artifact(record, name):
    required_keys(record, ("path", "sha256"), name)
    require(
        isinstance(record["path"], str) and record["path"] and sha256(record["sha256"]),
        f"{name}: expected a recorded path and SHA-256",
    )


def check_commands(commands, output):
    require(isinstance(commands, list), "commands: expected a list")
    invocations = {}
    for index, record in enumerate(commands):
        required_keys(record, SCHEMA["$defs"]["command"]["required"], f"commands[{index}]")
        name = record["name"]
        require(isinstance(name, str) and re.fullmatch("[a-z0-9-]+", name), f"invalid command name: {name!r}")
        require(name not in invocations, f"duplicate command: {name}")
        require(
            type(record["exit_code"]) is int
            and record["exit_code"] == 0
            and record.get("timed_out", False) is False,
            f"{name}: failed, timed out or incomplete",
        )
        argv, elapsed = record["command"], record["elapsed_seconds"]
        require(
            isinstance(argv, list)
            and argv
            and all(isinstance(arg, str) for arg in argv)
            and isinstance(record["cwd"], str)
            and record["cwd"],
            f"{name}: missing command or cwd",
        )
        invocations[name] = argv
        require(
            (type(elapsed) is int or (type(elapsed) is float and math.isfinite(elapsed))) and elapsed >= 0,
            f"{name}: invalid elapsed_seconds",
        )
        for stream in ("stdout", "stderr"):
            filename = f"{name}.{stream}"
            # CI paths identify the original run. Only open files in the supplied bundle.
            require(
                isinstance(record[stream], str)
                and Path(record[stream]).name == filename
                and (output / filename).is_file(),
                f"{name}: missing or misnamed {filename}",
            )
    return invocations


def check_receipts(label, actual, expected):
    require(isinstance(actual, list), f"{label}: expected ordered receipts")
    if actual != expected:
        first = next(
            (i for i, (a, b) in enumerate(zip(actual, expected)) if a != b), min(len(actual), len(expected))
        )
        raise ValueError(f"{label}: receipt {first + 1} differs (expected {len(expected)}, got {len(actual)})")


def check_rust_linkage(log):
    require(
        bool(log.strip()) and not re.search(r"lib(?:tent|mooncake|asio)|=>\s+not found", log),
        "rust-linkage: missing linkage or unexpected native dependency",
    )


def check_queues(output, driver):
    log = (output / f"{driver}-interrupted-read.stderr").read_text()
    pattern = r"^(resume|cut): stopped reader rx_queue=(\d+), stopped source tx_queue=(\d+)$"
    samples = re.findall(pattern, log, re.MULTILINE)
    require(
        [sample[0] for sample in samples] == ["resume", "cut"],
        f"{driver}-interrupted-read: expected resume and cut socket samples",
    )
    for outcome, rx, tx in samples:
        require(
            int(rx) >= (32 << 10) and int(rx) + int(tx) < (64 << 20),
            f"{driver}-interrupted-read: {outcome} did not observe a partial reply",
        )
    if driver == "python":
        require(
            re.search(
                r"^cut: transport I/O failed: (unexpected end of file|.+ \(os error 104\)), effect=none$",
                log,
                re.MULTILINE,
            ),
            "python-interrupted-read: missing drained READ failure diagnostic",
        )


def check_tebench(report, output, native_binary):
    schema = SCHEMA["properties"]["tebench"]
    required_keys(report, schema["required"], "tebench")
    summary = load_manifest(output / "tebench-bytes.stdout")
    for key in ("target", "commands", "payload_bytes", "guard_bytes_each_side", "corrupt_offset"):
        require(key in summary and report.get(key) == summary[key], f"tebench: inconsistent {key}")
    require(report["target"] == [native_binary, "--tebench-target"], "tebench: wrong target")
    target = (output / "tebench-target.stdout").read_text().splitlines()
    ready = re.fullmatch(r"READY (127\.0\.0\.1:[0-9]+)", target[0]) if target else None
    require(ready and target[1:] == ["FLIPPED 8191", "RESTORED", "CLOSED"], "tebench: incomplete target receipts")
    for key in ("payload_bytes", "guard_bytes_each_side", "corrupt_offset"):
        require(
            type(report[key]) is int and report[key] == schema["properties"][key]["const"], f"tebench: wrong {key}"
        )
    expected = schema["properties"]["commands"]["prefixItems"]
    require(
        isinstance(report["commands"], list) and len(report["commands"]) == len(expected),
        "tebench: expected seed, read, corrupt and restored in order",
    )
    for record, spec in zip(report["commands"], expected):
        case = spec["properties"]["case"]["const"]
        code = spec["properties"]["exit_code"]["const"]
        required_keys(record, ("case", "exit_code", "command"), f"tebench-{case}")
        require(
            record["case"] == case and type(record["exit_code"]) is int and record["exit_code"] == code,
            f"tebench-{case}: unexpected case or exit code",
        )
        argv = record["command"]
        operation = "write_seed" if case == "seed" else "read_verify"
        require(
            argv == initiator_command(report["path"], ready[1], operation),
            f"tebench-{case}: incorrect invocation for the advertised target",
        )
        stdout = (output / f"tebench-{case}.stdout").read_text()
        stderr = (output / f"tebench-{case}.stderr").read_text()
        if code:
            require("Inconsistent data detected" in stderr, "tebench: missing corrupt-byte rejection")
        else:
            check_workload_row(stdout, f"tebench-{case}")


def check_source(manifest, names, output):
    require(manifest["contract"] == CONTRACT, "reference contract differs from the frozen CPU contract")
    source = manifest["source"]
    required_keys(source, ("origin", "revision", "dirty"), "source")
    require(
        isinstance(source["revision"], str) and re.fullmatch("[a-f0-9]{40}", source["revision"]),
        "invalid source revision",
    )
    if source["origin"] == "git":
        require(
            type(source["dirty"]) is bool and {"source-revision", "source-status"} <= names,
            "git source: missing source receipt",
        )
        require(
            (output / "source-revision.stdout").read_text().strip() == source["revision"]
            and bool((output / "source-status.stdout").read_text().strip()) == source["dirty"],
            "git source receipts disagree",
        )
    else:
        require(
            source["origin"] == "cargo-package" and source["dirty"] is None, "invalid packaged source provenance"
        )
        require(
            not {"source-revision", "source-status"} & names, "packaged source conflicts with Git source commands"
        )

    inputs = manifest["input_sha256"]
    common = [CONTRACT["trace"], CONTRACT["expected"], "tests/reference/contract.json"]
    required_keys(inputs, common + ["tests/reference/run.py", "tests/reference/pending_read.py"], "input_sha256")
    require(all(sha256(digest) for digest in inputs.values()), "invalid input SHA-256")
    for path in common:
        require(
            hashlib.sha256((ROOT / path).read_bytes()).hexdigest() == inputs[path],
            f"{path}: input digest mismatch",
        )


def check_driver(driver, drivers, manifest, commands, output, trace, portable):
    artifact = {"rust": "rust_binary", "tent": "native_binary", "python": "python_wheel"}[driver]
    check_artifact(manifest[artifact], artifact)
    version = {"rust": "rustc", "tent": "cxx", "python": "python"}[driver]
    driver_paths = {
        "rust": ("rust_driver.rs", "pending_read.rs"),
        "tent": ("tent_driver.cpp", "tebench.py"),
        "python": ("python_driver.py",),
    }[driver]
    required_keys(manifest["input_sha256"], ["tests/reference/" + path for path in driver_paths], "input_sha256")
    for name in (*DRIVER_COMMANDS[driver], f"{driver}-trace"):
        require(name in commands, f"{driver}: missing command {name}")
    require(
        isinstance(manifest[version], str)
        and manifest[version]
        and (output / f"{version}-version.stdout").read_text().strip() == manifest[version],
        f"{version}: missing or inconsistent version receipt",
    )
    observed = (output / f"{driver}-trace.stdout").read_text().splitlines()
    check_receipts(f"{driver}-trace.stdout", [line for line in observed if line.startswith("OBS\t")], trace)
    invocation = commands[f"{driver}-trace"][:-1]
    require(commands[f"{driver}-trace"][-1].endswith("/" + CONTRACT["trace"]), f"{driver}-trace: wrong fixture")
    if driver == "python":
        venv = Path(commands["python-venv"][-1])
        interpreter = str(venv / "bin/python")
        install = commands["python-install"]
        require(
            install[0] == interpreter
            and install[-1] == manifest["python_wheel"]["path"]
            and "--no-index" in install
            and "--no-deps" in install,
            "python-install: wrong wheel or environment",
        )
        require(
            len(invocation) == 4
            and invocation[:3] == [interpreter, "-I", "-B"]
            and invocation[3].endswith("/tests/reference/python_driver.py"),
            "python-trace: wrong installed driver",
        )
        modules = [
            Path(line.removeprefix("Loaded OrbiTE from "))
            for line in (output / "python-trace.stderr").read_text().splitlines()
            if line.startswith("Loaded OrbiTE from ")
        ]
        require(
            len(modules) == 1
            and modules[0].is_relative_to(venv)
            and modules[0].parts[-3:] == ("site-packages", "orbite", "__init__.py"),
            "python-trace: missing wheel import receipt",
        )
    else:
        require(invocation == [manifest[artifact]["path"]], f"{driver}-trace: wrong executable")
    if driver == "rust":
        check_rust_linkage((output / "rust-linkage.stdout").read_text())
    for case in READ_CONTROLS:
        if portable and case == "interrupted_read":
            require(
                "python-interrupted-read" not in commands, "portable wheel records an interrupted READ command"
            )
            continue
        field = manifest[case]
        require(isinstance(field, dict) and set(field) == set(drivers), f"{case}: expected drivers {drivers}")
        expected = SCHEMA["properties"][case]["additionalProperties"]["const"]
        name = driver + "-" + case.replace("_", "-")
        require(name in commands, f"missing command {name}")
        argv = commands[name]
        require(
            len(argv) >= 5
            and argv[1:3] == ["-I", "-B"]
            and argv[3].endswith("/tests/reference/pending_read.py")
            and argv[4:] == READ_CONTROLS[case] + invocation,
            f"{name}: wrong READ control or driver",
        )
        check_receipts(f"{case}.{driver}", field[driver], expected)
        check_receipts(f"{name}.stdout", (output / f"{name}.stdout").read_text().splitlines(), expected)
    if not portable:
        check_queues(output, driver)
    if driver != "tent":
        diagnostic = (
            "old Describe and READ: EpochMismatch, zero bytes, drained, destination intact"
            if driver == "rust"
            else "old Describe and READ: source epoch does not match authoritative ownership, "
            "0 bytes, effect=none, drained, destination=eeeeeeeeee"
        )
        require(
            diagnostic in (output / f"{driver}-restart-read.stderr").read_text().splitlines(),
            f"{driver}-restart-read: missing stale-epoch receipt",
        )


def check_native(manifest, output):
    for name, key in (("tent-revision", "tent_revision"), ("pybind11-revision", "pybind11_revision")):
        require(
            (output / f"{name}.stdout").read_text().strip() == CONTRACT[key], f"{name}: frozen revision mismatch"
        )
    for name in ("tent-status", "tent-final-status"):
        require(not (output / f"{name}.stdout").read_text().strip(), f"{name}: dirty frozen source")
    check_artifact(manifest["tebench"], "tebench")
    for name, dependencies in (
        ("native", manifest["native_dependencies"]),
        ("tebench", manifest["tebench"].get("dependencies")),
    ):
        require(isinstance(dependencies, dict) and dependencies, f"{name}: missing native dependencies")
        for soname, artifact in dependencies.items():
            check_artifact(artifact, f"{name} dependency {soname}")
    required_keys(manifest["native_dependencies"], ("libtent_shared.so",), "native_dependencies")
    library = manifest["native_dependencies"]["libtent_shared.so"]
    require(
        library["sha256"] == manifest["native_binary"].get("library_sha256"), "frozen TENT library digest mismatch"
    )
    check_tebench(manifest["tebench"], output, manifest["native_binary"]["path"])


def validate(manifest, output, expected_status=None):
    """Return the verified success status, or raise with the missing or invalid evidence."""
    required_keys(manifest, ("schema_version", "status"), "manifest")
    require(
        type(manifest["schema_version"]) is int and manifest["schema_version"] == 1,
        "unsupported CPU manifest schema_version",
    )
    status = manifest["status"]
    if status == "running":
        raise ValueError("incomplete CPU run (running)")
    if status == "failed":
        raise ValueError(f"failed CPU run: {manifest.get('error') or 'no failure detail'}")
    require(isinstance(status, str) and status in PROFILES, f"unknown CPU result: {status!r}")
    require(expected_status is None or status == expected_status, f"expected {expected_status}, got {status}")
    require("error" not in manifest, f"{status}: passing manifest records an error")

    rule = PROFILES[status]
    required_keys(manifest, [*SCHEMA["required"], *rule["required"]], status)
    for key, constraint in rule["properties"].items():
        require(constraint is not False or key not in manifest, f"{status}: unexpected {key}")
    drivers = rule["patternProperties"]["_read$"]["required"]
    native = "tent" in drivers
    portable = "unavailable_controls" in rule["required"]
    for key in ("platform", "profile", "qualification"):
        require(isinstance(manifest[key], str) and manifest[key], f"missing {key}")
    require(manifest["platform"].startswith("Linux-") != portable, f"{status}: incompatible platform")
    require(
        manifest["native_executed"] is native and manifest["performance_claim"] is False,
        f"{status}: incompatible native or performance claim",
    )
    if portable:
        unavailable = manifest["unavailable_controls"]
        require(
            isinstance(unavailable, dict)
            and set(unavailable) == {"interrupted_read"}
            and isinstance(unavailable["interrupted_read"], str)
            and "/proc/net/tcp" in unavailable["interrupted_read"],
            "portable wheel: missing interrupted_read reason",
        )
    commands = check_commands(manifest["commands"], output)
    for other in DRIVER_COMMANDS.keys() - set(drivers):
        require(
            not any(name.startswith(other + "-") or name in DRIVER_COMMANDS[other] for name in commands),
            f"{status}: unexpected {other} command",
        )
    check_source(manifest, commands.keys(), output)
    trace = (ROOT / CONTRACT["expected"]).read_text().splitlines()
    require(
        type(manifest["observations"]) is int
        and len(trace) == manifest["observations"] == SCHEMA["properties"]["observations"]["const"],
        "shared trace observation count differs",
    )
    check_receipts(
        "negative_controls", manifest["negative_controls"], SCHEMA["properties"]["negative_controls"]["const"]
    )

    for driver in drivers:
        check_driver(driver, drivers, manifest, commands, output, trace, portable)
    if native:
        check_native(manifest, output)
    return status


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("manifest", type=Path)
    parser.add_argument("--expect", choices=PROFILES, help="require this exact passing profile")
    args = parser.parse_args()
    try:
        status = validate(load_manifest(args.manifest), args.manifest.parent, args.expect)
    except (OSError, ValueError, TypeError, KeyError, AttributeError) as error:
        print(f"{args.manifest}: {error}", file=sys.stderr)
        return 1
    print(f"{status}: verified {args.manifest}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
