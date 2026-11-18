"""Run the frozen tebench initiator against guarded native TCP memory."""

import json
import math
import os
import re
import resource
import select
import signal
import subprocess
import sys
from pathlib import Path


def check(binary: Path, driver: Path, output: Path) -> dict:
    block, batch, guard = 4096, 2, 32
    commands = []
    target_command = [str(driver), "--tebench-target"]
    with (
        (output / "tebench-target.stderr").open("w") as err,
        (output / "tebench-target.stdout").open("w") as transcript,
        subprocess.Popen(
            target_command,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=err,
            text=True,
        ) as target,
    ):

        def receipt(command=None):
            if command is not None:
                target.stdin.write(command + "\n")
                target.stdin.flush()
            if not select.select([target.stdout], [], [], 10)[0]:
                raise TimeoutError("tebench target receipt timed out")
            line = target.stdout.readline().rstrip("\n")
            print(line, file=transcript, flush=True)
            return line

        try:
            ready = re.fullmatch(r"READY (127\.0\.0\.1:[0-9]+)", receipt())
            if ready is None:
                raise RuntimeError(
                    "tebench target did not advertise its loopback segment"
                )
            common = [
                str(binary),
                "--backend=tent",
                "--metadata_type=p2p",
                "--seg_type=DRAM",
                "--xport_type=tcp",
                "--tent_transport_hint=tcp",
                "--duration=1",
                "--start_num_threads=1",
                "--max_num_threads=1",
                "--total_buffer_size=16384",
                f"--start_block_size={block}",
                f"--max_block_size={block}",
                f"--start_batch_size={batch}",
                f"--max_batch_size={batch}",
                f"--target_offset={guard}",
                f"--target_seg_name={ready[1]}",
            ]

            def initiate(label, operation, *, corrupt=False):
                command = [*common, f"--op_type={operation}"]
                stdout = output / f"tebench-{label}.stdout"
                stderr = output / f"tebench-{label}.stderr"
                with stdout.open("w") as out, stderr.open("w") as err:
                    result = subprocess.run(
                        command, stdout=out, stderr=err, timeout=25, check=False
                    )
                commands.append(
                    {"case": label, "command": command, "exit_code": result.returncode}
                )
                if corrupt:
                    if (
                        result.returncode != -signal.SIGABRT
                        or "Inconsistent data detected" not in stderr.read_text()
                    ):
                        raise RuntimeError(
                            f"{label} did not reject the corrupt byte; see {stderr}"
                        )
                    return
                result.check_returncode()
                rows = [
                    s.split()
                    for s in stdout.read_text().splitlines()
                    if s.split()[:2] == [str(block), str(batch)]
                ]
                if (
                    len(rows) != 1
                    or len(rows[0]) != 8
                    or not all(math.isfinite(float(x)) for x in rows[0][2:])
                    or float(rows[0][2]) <= 0
                ):
                    raise ValueError(
                        f"{label} produced no completed workload row; see {stdout}"
                    )

            initiate("seed", "write_seed")
            initiate("read", "read_verify")
            changed = block * batch - 1
            if receipt("flip") != f"FLIPPED {changed}":
                raise RuntimeError("target failed the guarded WRITE check")
            initiate("corrupt", "read_verify", corrupt=True)
            if receipt("restore") != "RESTORED":
                raise RuntimeError("target failed the single-byte restoration")
            initiate("restored", "read_verify")
            if receipt("close") != "CLOSED" or target.wait(timeout=10) != 0:
                raise RuntimeError("tebench target cleanup failed")
        finally:
            if target.poll() is None:
                target.kill()
                target.wait()
    return {
        "target": target_command,
        "commands": commands,
        "payload_bytes": block * batch,
        "guard_bytes_each_side": guard,
        "corrupt_offset": changed,
    }


if __name__ == "__main__":
    # The intentional native mismatch terminates with SIGABRT. Keep its log.
    resource.setrlimit(resource.RLIMIT_CORE, (0, 0))
    os.environ["GLOG_logtostderr"] = "1"
    print(json.dumps(check(*(Path(arg) for arg in sys.argv[1:])), indent=2))
