#!/usr/bin/env python3
"""Real managed startup wait, pinned TLS and cancellation. Never enables input."""
import json
import pathlib
import re
import socket
import subprocess
import sys
import tempfile
import time

binary = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else "target/release/peerglide").resolve()
children = []

def launch(args, log, managed=False):
    with log.open("w") as output:
        child = subprocess.Popen([str(binary), *args], stdin=subprocess.PIPE if managed else subprocess.DEVNULL,
                                 stdout=output, stderr=subprocess.STDOUT)
    children.append(child)
    return child

def records(log):
    result = []
    for line in log.read_text().splitlines():
        if line.startswith("{"):
            try:
                result.append(json.loads(line))
            except json.JSONDecodeError:
                pass  # Only while observing an in-progress bounded record.
    return result

def states(log):
    return [r["state"] for r in records(log) if "kind" not in r]

def wait_for(predicate, child, log, timeout=15):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if predicate():
            return
        if child.poll() is not None:
            raise AssertionError(log.read_text())
        time.sleep(0.02)
    raise TimeoutError(log.read_text())

def free_address():
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as sock:
        sock.bind(("127.0.0.1", 0))
        return "127.0.0.1:" + str(sock.getsockname()[1])

def no_input(log, terminal=None):
    data = records(log)
    expected = {"version": 1, "kind": "input", "state": "Disabled"}
    inputs = [r for r in data if r.get("kind") == "input"]
    final = [expected, {**expected, "terminal": True}]
    if terminal is None:
        assert inputs in ([expected], final), log.read_text()
    else:
        assert inputs == (final if terminal else [expected]), log.read_text()

def terminate(child):
    if child.poll() is None:
        child.terminate()
        child.wait(timeout=5)

with tempfile.TemporaryDirectory(prefix="peerglide-initial-wait-") as folder:
    root = pathlib.Path(folder)
    a, b, c = [root / name for name in ("a", "b", "c")]
    for identity in (a, b, c):
        subprocess.run([str(binary), "init", "--identity", str(identity)], check=True, capture_output=True)
    pin = root / "enrolled.der"
    pin.write_bytes((b / "node.crt.der").read_bytes())
    serial = 0

    def connector(address, seconds):
        global serial
        serial += 1
        log = root / ("client-" + str(serial) + ".log")
        child = launch(["connect", address, "--identity", str(a), "--peer-cert", str(pin),
                        "--bind", "127.0.0.1:0", "--managed", "--wait-for-peer-secs", str(seconds)], log, True)
        wait_for(lambda: "WaitingForPeer" in states(log), child, log)
        no_input(log)
        return child, log

    def listener(identity, address):
        log = root / ("server-" + str(len(children)) + ".log")
        child = launch(["listen", "--identity", str(identity), "--peer-cert", str(a / "node.crt.der"),
                        "--bind", address, "--managed"], log, True)
        wait_for(lambda: "Listening on " in log.read_text(), child, log)
        return child

    def graceful_stop(child):
        if child.poll() is None:
            child.stdin.write(b"stop\n")
            child.stdin.flush()
        assert child.wait(timeout=5) == 0, "cooperative no-input server stop failed"

    try:
        address = free_address()
        client, log = connector(address, 15)
        wait_for(lambda: states(log).count("WaitingForPeer") >= 2, client, log)
        server = listener(b, address)
        wait_for(lambda: "Authenticated" in states(log), client, log)
        graceful_stop(server)
        assert client.wait(timeout=10) == 0, log.read_text()
        assert states(log).count("Authenticated") == 1 and "Reconnecting" not in states(log)
        no_input(log, True)
        print("PASS: late peer connects after initial failure; disconnect does not enable reconnect")

        address = free_address()
        server = listener(b, address)
        client, log = connector(address, 3)
        wait_for(lambda: "Authenticated" in states(log), client, log)
        time.sleep(3.2)
        assert client.poll() is None, log.read_text()
        client.stdin.write(b"stop\n"); client.stdin.flush()
        assert client.wait(timeout=5) == 0, log.read_text()
        no_input(log, True)
        terminate(server)
        print("PASS: startup deadline cannot cancel an established session")

        address = free_address()
        server = listener(b, address)
        client, log = connector(address, 15)
        wait_for(lambda: "Authenticated" in states(log), client, log)
        terminate(server)
        assert client.wait(timeout=10) != 0, log.read_text()
        assert states(log)[-1] == "Failed" and "Reconnecting" not in states(log)
        assert states(log).count("Authenticated") == 1
        no_input(log, True)
        print("PASS: unclean peer loss stays a failure and does not enable reconnect")

        client, log = connector(free_address(), 2)
        assert client.wait(timeout=6) != 0, log.read_text()
        assert states(log)[-1] == "PeerWaitExpired", log.read_text()
        assert "Authenticated" not in states(log)
        no_input(log, True)
        print("PASS: one startup budget expires without input or fabricated authentication")

        client, log = connector(free_address(), 15)
        client.stdin.write(b"stop\n"); client.stdin.flush()
        assert client.wait(timeout=5) == 0, log.read_text()
        assert states(log)[-1] == "Stopped"
        no_input(log, True)
        print("PASS: managed stop cancels initial negotiation")

        client, log = connector(free_address(), 15)
        pin.write_bytes((c / "node.crt.der").read_bytes())
        assert client.wait(timeout=12) != 0, log.read_text()
        assert "identity files changed" in log.read_text() and "Authenticated" not in states(log)
        no_input(log, True)
        pin.write_bytes((b / "node.crt.der").read_bytes())
        print("PASS: changed enrolled pin stops initial retry")

        address = free_address()
        server = listener(c, address)
        client, log = connector(address, 15)
        assert client.wait(timeout=8) != 0, log.read_text()
        assert "Authenticated" not in states(log) and states(log)[-1] == "Failed"
        no_input(log, True)
        terminate(server)
        print("PASS: TLS authentication failure is not a startup retry")

        client, log = connector(free_address(), 15)
        (a / "node.key.der").unlink()  # This test owns this temporary synthetic identity.
        assert client.wait(timeout=12) != 0, log.read_text()
        assert "identity files changed" in log.read_text() and "Authenticated" not in states(log)
        no_input(log, True)
        print("PASS: unusable local key stops initial retry")
    finally:
        for child in children:
            terminate(child)
