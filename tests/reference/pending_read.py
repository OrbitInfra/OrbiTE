"""Drive public TCP readers and their private loopback sources."""

import os
import signal
import subprocess
import sys
import time
from contextlib import ExitStack
from pathlib import Path


class Drivers(ExitStack):
    def __init__(self, command):
        super().__init__()
        self.command = command

    def start(self, *args):
        child = self.enter_context(
            subprocess.Popen(
                [*self.command, *args],
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                text=True,
            )
        )
        # Kill before Popen.__exit__ waits, including a child still in SIGSTOP.
        self.callback(child.kill)
        return child

    def source(self, *args):
        child = self.start(*args)
        ready = line(child)
        if not ready.startswith("READY 127.0.0.1:"):
            raise RuntimeError(f"source did not advertise a loopback peer: {ready!r}")
        return child, ready.removeprefix("READY ")


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


def stop(child):
    child.send_signal(signal.SIGSTOP)
    _, status = os.waitpid(child.pid, os.WUNTRACED)
    if not os.WIFSTOPPED(status) or os.WSTOPSIG(status) != signal.SIGSTOP:
        raise RuntimeError(f"driver did not stop: {child.args}")


def check(driver):
    with Drivers(driver) as peers:
        slow, slow_peer = peers.source("--pending-source", "first...")
        fast, fast_peer = peers.source("--pending-source", "two.....")
        reader = peers.start("--pending-reads", slow_peer, fast_peer)
        expect(reader, "OPEN")
        stop(slow)
        send(reader, "stopped")
        expect(reader, "OBS\tRESULT\tsecond\tcompleted\t1\t3")
        expect(reader, "OBS\tBYTES\tsecond\tee74776feeeeeeee")
        expect(reader, "PENDING first")
        expect(reader, "PENDING first later")
        slow.send_signal(signal.SIGCONT)
        send(reader, "resumed")
        expect(reader, "OBS\tRESULT\tlater\tcompleted\t1\t2")
        expect(reader, "OBS\tBYTES\tlater\tee7374eeeeeeeeee")
        expect(reader, "OBS\tRESULT\tfirst\tcompleted\t1\t5")
        expect(reader, "OBS\tBYTES\tfirst\tee6669727374eeee")
        finish(reader)
        for target in (slow, fast):
            send(target, "close")
            finish(target)
        print("CLEANUP", flush=True)


def socket_queues(source):
    # One snapshot of both directions of this source's established IPv4 connections.
    queues = {}
    for row in Path("/proc/net/tcp").read_text().splitlines()[1:]:
        _, local, peer, state, sizes, *_ = row.split()
        if state == "01" and source in (local, peer):
            queues[local, peer] = tuple(int(n, 16) for n in sizes.split(":"))
    return queues


def wait_for(label, sample):
    end = time.monotonic() + 5
    while time.monotonic() < end:
        value = sample()
        if value:
            return value
        time.sleep(0.005)
    raise RuntimeError(f"missing TCP queue observation: {label}")


def interrupted_read(driver, cut):
    length = 64 << 20
    min_reply = 32 << 10
    outcome = "cut" if cut else "resume"
    with Drivers(driver) as peers:
        source, peer = peers.source("--bulk-read-source")
        reader = peers.start("--interrupted-read", peer, outcome)
        expect(reader, "OBS\tRESULT\twarmup\tcompleted\t1\t1")
        expect(reader, "OPEN")
        # No READ reply can arrive until the requester has submitted and stopped.
        stop(source)
        send(reader, "submit")
        expect(reader, "PENDING read")
        port = int(peer.split()[0].split(":")[1])
        server = f"0100007F:{port:04X}"

        def request_peer():
            queued = [
                remote
                for (local, remote), (_, rx) in socket_queues(server).items()
                if local == server and rx
            ]
            if len(queued) > 1:
                raise RuntimeError("multiple connections have an unread request")
            return next(iter(queued), None)

        client = wait_for("request queued at stopped source", request_peer)
        stop(reader)
        source.send_signal(signal.SIGCONT)

        def partial_reply():
            rx = socket_queues(server).get((client, server), (0, 0))[1]
            return rx if rx >= min_reply else None

        wait_for("bulk reply queued at stopped reader", partial_reply)
        stop(source)
        # /proc visits sockets in arbitrary order. ACKs can advance while both
        # processes are stopped. Sample TX first so bytes moving to RX can
        # only make this upper bound larger.
        tx = socket_queues(server)[server, client][0]
        rx = socket_queues(server)[client, server][1]
        if not (min_reply <= rx and tx + rx < length):
            raise RuntimeError(
                f"cannot prove a partial {length}-byte READ: tx={tx}, rx={rx}"
            )
        print(
            f"{outcome}: stopped reader rx_queue={rx}, stopped source tx_queue={tx}",
            file=sys.stderr,
            flush=True,
        )
        reader.send_signal(signal.SIGCONT)

        def prefix_consumed():
            return (
                socket_queues(server)[server, client][0] == 0
                and socket_queues(server)[client, server][1] == 0
            )

        wait_for(
            "both established queues empty with source still stopped", prefix_consumed
        )
        send(reader, "prefix")
        expect(reader, "PENDING prefix")
        if cut:
            source.kill()
            if source.wait() != -signal.SIGKILL:
                raise RuntimeError("source did not exit on SIGKILL")
        else:
            source.send_signal(signal.SIGCONT)
        send(reader, outcome)
        status, committed = ("failed", 0) if cut else ("completed", length)
        expect(reader, f"OBS\tRESULT\t{outcome}\t{status}\t{committed}")
        expect(reader, f"OBS\tBYTES\t{outcome}\t{committed}\tguards-intact")
        finish(reader)
        if not cut:
            send(source, "close")
            finish(source)
        print("CLEANUP", flush=True)


if __name__ == "__main__":
    signal.signal(signal.SIGTERM, lambda signum, _: sys.exit(128 + signum))
    if sys.argv[1] == "--interrupted":
        for cut in (False, True):
            interrupted_read(sys.argv[2:], cut)
    else:
        check(sys.argv[1:])
