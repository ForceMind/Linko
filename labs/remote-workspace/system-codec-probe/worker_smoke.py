"""Exercise the native codec worker over its inherited control pipes."""

import os
import re
import selectors
import signal
import struct
import subprocess
import sys
import tempfile
import time
from contextlib import contextmanager
from pathlib import Path


RECORD = struct.Struct(">4sBBHQ")
START, READY, STOP, STOPPED = range(1, 5)


def message(kind, generation, version=1):
    return RECORD.pack(b"LRW4", version, kind, 0, generation)


def expect_message(child, kind, generation, timeout):
    expected = message(kind, generation)
    received = bytearray()
    deadline = time.monotonic() + timeout
    with selectors.DefaultSelector() as poll:
        poll.register(child.stdout, selectors.EVENT_READ)
        while len(received) < len(expected):
            remaining = deadline - time.monotonic()
            if remaining <= 0 or not poll.select(remaining):
                raise TimeoutError(f"Codec worker did not return event {kind}")
            chunk = os.read(child.stdout.fileno(), len(expected) - len(received))
            if not chunk:
                raise RuntimeError(f"Codec worker closed before event {kind}")
            received.extend(chunk)
    if received != expected:
        raise AssertionError(f"Codec worker returned a mismatched event {kind}")


@contextmanager
def worker(binary, log, luma):
    with log.open("w") as output:
        child = subprocess.Popen(
            [binary, "--worker-fixture", str(luma)],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=output,
        )
    try:
        yield child
    finally:
        if child.poll() is None:
            child.kill()
            child.wait(timeout=10)
        child.stdin.close()
        child.stdout.close()
        print(log.read_text(), end="", flush=True)


def exercise(binary, log, luma, generation, *, abrupt=False, stale_stop=None):
    with worker(binary, log, luma) as child:
        start = message(START, generation)
        child.stdin.write(start[:3])
        child.stdin.flush()
        child.stdin.write(start[3:])
        child.stdin.flush()
        expect_message(child, READY, generation, timeout=90)
        if child.poll() is not None:
            raise AssertionError("Codec worker left its live session before the stop check")

        expected = 0
        if abrupt:
            child.kill()
            expected = -signal.SIGKILL
        else:
            requested = generation if stale_stop is None else stale_stop
            child.stdin.write(message(STOP, requested))
            child.stdin.flush()
            if stale_stop is None:
                expect_message(child, STOPPED, generation, timeout=10)
            else:
                expected = 1

        result = child.wait(timeout=10)
        lines = log.read_text().splitlines()
        released = f"VideoToolbox worker released: luma={luma}."
        if result != expected or (released in lines) != (not abrupt):
            raise AssertionError(f"Unexpected codec worker exit: {result}, expected {expected}")
        if child.stdout.read(1):
            raise AssertionError("Codec worker sent an unexpected event after session close")
        if stale_stop is not None and "VideoToolbox worker control: mismatched generation." not in lines:
            raise AssertionError("Codec worker did not reject the previous generation")

        snapshots = re.findall(
            r"^VideoToolbox memory: stopped=(\d+) rss_bytes=(\d+)\n"
            r"Loopback VM absolute: IOAccelerator resident_bytes=(\d+)$",
            "\n".join(lines),
            re.MULTILINE,
        )
        if [int(cycle) for cycle, _, _ in snapshots] != [0, 10, 100]:
            raise AssertionError("Codec worker did not report all three resource samples")
        print(f"Codec worker reaped: luma={luma}, exit={result}, pid={child.pid}.", flush=True)
        return child.pid


def reject_start(binary, log, payload, diagnostic):
    with worker(binary, log, luma=80) as child:
        child.stdin.write(payload)
        child.stdin.close()
        if child.wait(timeout=10) != 1 or child.stdout.read(1):
            raise AssertionError("Codec worker accepted an invalid first message")
        log_text = log.read_text()
        if diagnostic not in log_text or "VideoToolbox memory:" in log_text:
            raise AssertionError("Codec worker did not reject the first message before codec startup")


def main(binary):
    with tempfile.TemporaryDirectory(prefix="linko-vt-workers-") as directory:
        root = Path(directory)
        generation = 0x0102030405060708
        reject_start(binary, root / "version.log", message(START, generation, version=2), "invalid header")
        reject_start(binary, root / "truncated.log", message(START, generation)[:-1], "incomplete message")
        reject_start(binary, root / "out-of-order.log", message(STOP, generation), "invalid header")
        reject_start(binary, root / "zero-generation.log", message(START, 0), "missing generation")
        normal = exercise(binary, root / "normal.log", 80, generation)
        interrupted = exercise(binary, root / "interrupted.log", 144, generation + 1, abrupt=True)
        replacement = exercise(binary, root / "replacement.log", 208, generation + 2)
        stale = exercise(binary, root / "stale-stop.log", 160, generation + 3, stale_stop=generation)
        if len({normal, interrupted, replacement, stale}) != 4:
            raise AssertionError("Expected distinct codec fixture processes")
    print("PASS: worker control versions and generations; graceful stop, forced exit and replacement decode")


if __name__ == "__main__":
    main(sys.argv[1])
