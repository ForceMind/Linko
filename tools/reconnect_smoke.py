#!/usr/bin/env python3
"""Real loopback reconnect, pin replacement rejection and cancellable backoff.
No OS input or public services are used.
"""
import json
import pathlib
import re
import subprocess
import sys
import tempfile
import time

binary = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else "target/release/peerglide").resolve()
children = []

def launch(args, log, managed=False):
    with log.open("w") as output:
        child = subprocess.Popen([str(binary), *args], stdin=subprocess.PIPE if managed else subprocess.DEVNULL, stdout=output, stderr=subprocess.STDOUT)
    children.append(child)
    return child

def stop(child):
    if child.poll() is None:
        child.terminate()
        child.wait(timeout=5)

def wait_for(predicate, child, log, timeout=25):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if predicate():
            return
        if child.poll() is not None:
            raise AssertionError(log.read_text())
        time.sleep(0.05)
    raise TimeoutError(log.read_text())

def states(log):
    result = []
    if not log.exists():
        return result
    for line in log.read_text().splitlines():
        if line.startswith("{"):
            try:
                record = json.loads(line)
                if "kind" not in record:
                    result.append(record["state"])
            except (json.JSONDecodeError, KeyError):
                pass  # A concurrent reader can observe the final line before flush.
    return result

def assert_input_disabled(log, finished):
    records = [json.loads(line) for line in log.read_text().splitlines()
               if line.startswith("{")]
    inputs = [record for record in records if record.get("kind") == "input"]
    disabled = {"version": 1, "kind": "input", "state": "Disabled"}
    expected = [disabled]
    if finished:
        expected.append({**disabled, "terminal": True})
    assert inputs == expected, log.read_text()

def switch_states(log):
    result = []
    for line in log.read_text().splitlines():
        if line.startswith("{"):
            try:
                record = json.loads(line)
                if record.get("kind") == "switch":
                    assert set(record) == {"version", "kind", "state"} and record["version"] == 1
                    assert record["state"] in {"Idle", "PeerUnavailable"}, "no native switch may be reported by this smoke"
                    result.append(record["state"])
            except json.JSONDecodeError:
                pass  # Live log snapshot may end during one bounded record.
    return result

with tempfile.TemporaryDirectory(prefix="peerglide-reconnect-") as work:
    root = pathlib.Path(work)
    a, b, c = [root / name for name in ("a", "b", "c")]
    fingerprints = {}
    for identity in (a, b, c):
        created = subprocess.run(
            [str(binary), "init", "--identity", str(identity)],
            check=True, capture_output=True, text=True,
        )
        match = re.search(r"(?m)^BLAKE3: ([0-9a-f]{64})$", created.stdout)
        assert match, created.stdout
        fingerprints[identity] = match[1]
    pin = root / "enrolled.der"
    original_pin = (b / "node.crt.der").read_bytes()
    pin.write_bytes(original_pin)
    server_log, client_log = root / "server.log", root / "client.log"
    client_args = [
        "connect", "127.0.0.1:9", "--identity", str(a), "--peer-cert", str(pin),
        "--bind", "127.0.0.1:0", "--managed", "--reconnect",
        "--expected-local-fingerprint", fingerprints[a],
        "--expected-peer-fingerprint", fingerprints[b],
    ]
    try:
        pin.write_bytes((c / "node.crt.der").read_bytes())
        changed_peer = launch(client_args, client_log, managed=True)
        assert changed_peer.wait(timeout=5) != 0, client_log.read_text()
        assert "enrolled peer identity differs from the GUI snapshot" in client_log.read_text()
        assert states(client_log) == ["Failed"], client_log.read_text()

        pin.write_bytes(original_pin)
        changed_local_args = client_args.copy()
        changed_local_args[changed_local_args.index("--identity") + 1] = str(c)
        changed_local = launch(changed_local_args, client_log, managed=True)
        assert changed_local.wait(timeout=5) != 0, client_log.read_text()
        assert "local identity differs from the GUI snapshot" in client_log.read_text()
        assert states(client_log) == ["Failed"], client_log.read_text()
        print("PASS: a changed launch identity is rejected before the first connection attempt")

        def server(identity, bind):
            child = launch(["listen", "--identity", str(identity), "--peer-cert", str(a / "node.crt.der"), "--bind", bind], server_log)
            wait_for(lambda: re.search(r"Listening on (127\.0\.0\.1:\d+)", server_log.read_text()), child, server_log)
            return child
        first = server(b, "127.0.0.1:0")
        address = re.search(r"Listening on (127\.0\.0\.1:\d+)", server_log.read_text())[1]
        client_args[1] = address
        client = launch(client_args, client_log, managed=True)
        wait_for(lambda: states(client_log).count("Authenticated") >= 1, client, client_log)
        stop(first)
        wait_for(lambda: "Reconnecting" in states(client_log), client, client_log)
        second = server(b, address)
        wait_for(lambda: states(client_log).count("Authenticated") >= 2, client, client_log)
        wait_for(lambda: switch_states(client_log).count("PeerUnavailable") == 2, client, client_log)
        assert_input_disabled(client_log, finished=False)
        print("PASS: independent processes reconnect and authenticate the same pinned peer")
        retry_count = states(client_log).count("Reconnecting")
        stop(second)
        wait_for(lambda: states(client_log).count("Reconnecting") > retry_count, client, client_log)
        # A different valid identity is made available at the same address and
        # its certificate replaces the enrolled file. It must not become trusted.
        pin.write_bytes((c / "node.crt.der").read_bytes())
        third = server(c, address)
        assert client.wait(timeout=25) != 0, client_log.read_text()
        assert "identity changed during reconnect" in client_log.read_text(), client_log.read_text()
        assert states(client_log).count("Authenticated") == 2, client_log.read_text()
        assert_input_disabled(client_log, finished=True)
        assert switch_states(client_log).count("PeerUnavailable") == 2
        assert switch_states(client_log)[-1] == "Idle"
        print("PASS: changing a pin cannot silently enroll a replacement identity")
        stop(third)
        pin.write_bytes(original_pin)
        waiting = launch(client_args, client_log, managed=True)
        wait_for(lambda: "Reconnecting" in states(client_log), waiting, client_log)
        waiting.stdin.write(b"stop\n")
        waiting.stdin.flush()
        assert waiting.wait(timeout=5) == 0, client_log.read_text()
        assert states(client_log)[-1] == "Stopped", client_log.read_text()
        assert_input_disabled(client_log, finished=True)
        assert switch_states(client_log) == ["Idle"]
        print("PASS: managed stop cancels reconnection backoff")
    finally:
        for child in children:
            stop(child)
            if child.stdin:
                try:
                    child.stdin.close()
                except OSError:
                    pass
