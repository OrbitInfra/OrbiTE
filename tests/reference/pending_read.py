"""Hold one loopback source stopped while the same requester drains another READ."""

from contextlib import ExitStack
import os
import signal
import subprocess
import sys


def check(driver):
    # run.py gives this controller and all three children one process-group timeout.
    with ExitStack() as children:
        def start(*args):
            child = children.enter_context(subprocess.Popen(
                [*driver, *args], stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True
            ))
            # Kill before Popen.__exit__ waits, including a child still in SIGSTOP.
            children.callback(child.kill)
            return child

        def line(child):
            value = child.stdout.readline()
            if not value.endswith("\n"):
                raise RuntimeError(f"missing receipt from {child.args}: {value!r}")
            return value.rstrip("\n")

        def expect(child, wanted):
            actual = line(child)
            if actual != wanted:
                raise RuntimeError(f"expected {wanted!r}, received {actual!r}")
            print(actual, flush=True)

        def send(child, command):
            child.stdin.write(command + "\n")
            child.stdin.flush()

        def finish(child):
            extra, _ = child.communicate()
            if child.returncode or extra:
                raise RuntimeError(f"{child.args} exited {child.returncode}: {extra!r}")

        def source(payload):
            child = start("--pending-source", payload)
            ready = line(child)
            if not ready.startswith("READY 127.0.0.1:"):
                raise RuntimeError(f"source did not advertise a loopback peer: {ready!r}")
            return child, ready.removeprefix("READY ")

        slow, slow_peer = source("first...")
        fast, fast_peer = source("two.....")
        reader = start("--pending-reads", slow_peer, fast_peer)
        expect(reader, "OPEN")
        slow.send_signal(signal.SIGSTOP)
        _, status = os.waitpid(slow.pid, os.WUNTRACED)
        if not os.WIFSTOPPED(status) or os.WSTOPSIG(status) != signal.SIGSTOP:
            raise RuntimeError("first source did not stop")
        send(reader, "stopped")
        expect(reader, "OBS\tRESULT\tsecond\tcompleted\t1\t3")
        expect(reader, "OBS\tBYTES\tsecond\tee74776feeeeeeee")
        expect(reader, "PENDING first")
        slow.send_signal(signal.SIGCONT)
        send(reader, "resumed")
        expect(reader, "OBS\tRESULT\tfirst\tcompleted\t1\t5")
        expect(reader, "OBS\tBYTES\tfirst\tee6669727374eeee")
        finish(reader)
        for target in (slow, fast):
            send(target, "close")
            finish(target)
        print("CLEANUP", flush=True)


if __name__ == "__main__":
    signal.signal(signal.SIGTERM, lambda signum, _: sys.exit(128 + signum))
    check(sys.argv[1:])
