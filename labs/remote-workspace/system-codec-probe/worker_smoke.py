"""Verify retirement and replacement of native codec fixture processes."""

import re
import signal
import subprocess
import sys
import tempfile
import time
from pathlib import Path


def wait_ready(child, log, ready):
    deadline = time.monotonic() + 90
    while time.monotonic() < deadline:
        if ready in log.read_text().splitlines() and child.poll() is None:
            return
        if child.poll() is not None:
            raise RuntimeError(f"Codec worker exited before readiness: {child.returncode}")
        time.sleep(0.05)
    raise TimeoutError("Codec worker did not become ready")


def exercise(binary, log, luma, abrupt):
    with log.open("w") as output:
        child = subprocess.Popen(
            [binary, "--worker-fixture", str(luma)],
            stdin=subprocess.PIPE,
            stdout=output,
            stderr=subprocess.STDOUT,
        )
    ready = f"VideoToolbox worker ready: luma={luma}."
    closed = f"VideoToolbox worker closed: luma={luma}."
    try:
        wait_ready(child, log, ready)
        if abrupt:
            child.kill()
        else:
            child.stdin.write(b"stop\n")
            child.stdin.flush()
        expected = -signal.SIGKILL if abrupt else 0
        result = child.wait(timeout=10)
        lines = log.read_text().splitlines()
        if result != expected or (closed in lines) != (not abrupt):
            raise AssertionError(f"Unexpected codec worker exit: {result}, expected {expected}")
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
    finally:
        if child.poll() is None:
            child.kill()
            child.wait(timeout=10)
        child.stdin.close()
        print(log.read_text(), end="", flush=True)


def main(binary):
    with tempfile.TemporaryDirectory(prefix="linko-vt-workers-") as directory:
        root = Path(directory)
        normal = exercise(binary, root / "normal.log", 80, abrupt=False)
        interrupted = exercise(binary, root / "interrupted.log", 144, abrupt=True)
        replacement = exercise(binary, root / "replacement.log", 208, abrupt=False)
        if len({normal, interrupted, replacement}) != 3:
            raise AssertionError("Expected distinct codec fixture processes")
    print("PASS: graceful stop, live codec termination, and fresh replacement decode")


if __name__ == "__main__":
    main(sys.argv[1])
