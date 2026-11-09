#!/usr/bin/env python3
"""Real private-pipe layout updates over an authenticated NoNative session.

Fresh identities and loopback only. This does not validate physical screen edges.
"""
import json
import pathlib
import re
import subprocess
import sys
import tempfile
import time


def records(log):
    result = []
    for line in log.read_text(encoding="utf-8").splitlines():
        if line.startswith("{"):
            try:
                result.append(json.loads(line))
            except json.JSONDecodeError:
                pass  # A live file can end partway through one bounded record.
    return result


def wait_for(predicate, child, log):
    deadline = time.monotonic() + 15
    while time.monotonic() < deadline:
        if predicate():
            return
        if child.poll() is not None:
            raise AssertionError(log.read_text(encoding="utf-8"))
        time.sleep(0.05)
    raise TimeoutError(log.read_text(encoding="utf-8"))


def send(child, data):
    child.stdin.write(data)
    child.stdin.flush()


def command(request, edge):
    return {"version": 1, "kind": "layout", "request": request, "edge": edge}


def main():
    binary = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else "target/release/peerglide").resolve()
    children = []
    with tempfile.TemporaryDirectory(prefix="linko-layout-") as work:
        root = pathlib.Path(work)
        identities = [root / "a", root / "b"]
        for identity in identities:
            subprocess.run([str(binary), "init", "--identity", str(identity)],
                           check=True, capture_output=True)

        def launch(args, name):
            log = root / (name + ".log")
            with log.open("wb") as output:
                child = subprocess.Popen([str(binary), *args, "--managed"],
                                         stdin=subprocess.PIPE, stdout=output,
                                         stderr=subprocess.STDOUT)
            children.append(child)
            return child, log

        try:
            listener, listener_log = launch([
                "listen", "--identity", str(identities[0]), "--peer-cert",
                str(identities[1] / "node.crt.der"), "--bind", "127.0.0.1:0",
            ], "listener")
            pattern = r"Listening on (127\.0\.0\.1:\d+)"
            wait_for(lambda: re.search(pattern, listener_log.read_text()), listener, listener_log)
            address = re.search(pattern, listener_log.read_text())[1]
            connector, connector_log = launch([
                "connect", address, "--identity", str(identities[1]), "--peer-cert",
                str(identities[0] / "node.crt.der"), "--bind", "127.0.0.1:0",
            ], "connector")
            for child, log in [(listener, listener_log), (connector, connector_log)]:
                wait_for(lambda: any(r.get("state") == "Authenticated" for r in records(log)),
                         child, log)
                # Oversized/unknown input must drain without hiding the next command.
                send(child, b"x" * 4096 + b'\n{"version":2,"kind":"layout"}\n')
                expected = []
                for request, edge in enumerate(["Left", "Top", "Bottom", "Right"], 1):
                    update = command(request, edge)
                    send(child, (json.dumps(update) + "\n").encode())
                    reply = {**update, "applied": True}
                    expected.append(reply)
                    wait_for(lambda: reply in records(log), child, log)
                actual = records(log)
                assert [r for r in actual if r.get("kind") == "layout"] == expected, actual
                assert sum(r.get("state") == "Authenticated" for r in actual) == 1, actual
                assert [r["state"] for r in actual if r.get("kind") == "input"] == ["Disabled"], actual
                assert child.poll() is None, actual

            # Stop must remain reachable behind a drained oversized command.
            send(connector, b"x" * 4096 + b"\nstop\n")
            assert connector.wait(timeout=10) == 0, connector_log.read_text()
            assert listener.wait(timeout=10) == 0, listener_log.read_text()
            for log in [listener_log, connector_log]:
                actual = records(log)
                assert actual[-1] == {"version": 1, "state": "Stopped"}, actual
                assert [r["state"] for r in actual if r.get("kind") == "input"] == [
                    "Disabled", "Disabled"], actual
                assert any(r.get("terminal") is True for r in actual), actual
            print("PASS: two authenticated workers apply all four local directions without reconnect;")
            print("      bounded invalid commands drain, managed stop exits cleanly; no native input")
        finally:
            for child in children:
                if child.poll() is None:
                    child.kill()
                    child.wait(timeout=5)
                if child.stdin:
                    child.stdin.close()


if __name__ == "__main__":
    main()
