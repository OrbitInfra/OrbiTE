"""Run with python -I -m orbite.quickstart from an installed wheel."""
import json
from pathlib import Path
import subprocess
import sys
import tempfile

import orbite


def check(condition, message):
    if not condition:
        raise RuntimeError(message)


def transfer(engine, operation, target, request):
    with engine.allocate_batch(1) as batch:
        batch.submit(operation, target, [request])
        tasks = batch.wait(30)
        check(len(tasks) == 1 and tasks[0].state == orbite.TaskState.COMPLETED
              and tasks[0].transferred_bytes == request.length and tasks[0].locally_drained,
              f"incomplete transfer: {tasks}")


def source():
    with orbite.TransferEngine(orbite.EngineConfig()) as engine:
        data = bytearray(b"LLsourceRR")
        with engine.register_buffer(data) as memory:
            print(json.dumps([engine.endpoint(), engine.source_epoch(), memory.address]), flush=True)
            check(sys.stdin.readline().strip() == "done", "requester did not complete")
            check(data == b"LLtargetRR", "TCP WRITE or source guard mismatch")


def main():
    if sys.argv[1:] == ["--source"]:
        source()
        return
    with subprocess.Popen([sys.executable, "-I", "-m", "orbite.quickstart", "--source"],
                          stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True) as child:
        try:
            endpoint, epoch, remote = json.loads(child.stdout.readline())
            with orbite.TransferEngine(orbite.EngineConfig()) as engine:
                data = bytearray(b"xx------yy")
                with engine.register_buffer(data) as memory, engine.open_segment(endpoint, epoch) as peer:
                    request = orbite.TransferRequest(memory.address + 2, remote + 2, 6, orbite.Transport.TCP)
                    transfer(engine, orbite.TransferOp.READ, peer, request)
                    check(data == b"xxsourceyy", "TCP READ or destination guard mismatch")
                    data[2:8] = b"target"
                    transfer(engine, orbite.TransferOp.WRITE, peer, request)
                child.communicate("done\n", timeout=30)
                check(child.returncode == 0, "source verification failed")
                print("TCP: two-process READ/WRITE and guards verified")
                with tempfile.TemporaryDirectory(prefix="orbite-quickstart-") as directory:
                    path = Path(directory) / "data"
                    path.write_bytes(b"LL------RR")
                    data = bytearray(b"xxstoragyy")
                    with engine.register_buffer(data) as memory, engine.open_file_segment(str(path)) as file:
                        request = orbite.TransferRequest(memory.address + 2, 2, 6, orbite.Transport.IO_URING)
                        transfer(engine, orbite.TransferOp.WRITE, file, request)
                        check(path.read_bytes() == b"LLstoragRR", "file WRITE or guard mismatch")
                        data[2:8] = b"------"
                        transfer(engine, orbite.TransferOp.READ, file, request)
                        check(data == b"xxstoragyy", "file READ or guard mismatch")
                print("io_uring: DRAM/file READ/WRITE and guards verified")
        finally:
            if child.poll() is None:
                child.kill()
                child.wait()


if __name__ == "__main__":
    main()
