#!/usr/bin/env python3
"""Exercise two real CLI processes without OS input capture or public services."""
import json
import pathlib
import re
import subprocess
import sys
import tempfile
import time

def managed_states(output):
    records = [json.loads(line) for line in output.splitlines() if line.startswith("{")]
    inputs = [record for record in records if record.get("kind") == "input"]
    disabled = {"version": 1, "kind": "input", "state": "Disabled"}
    terminal = {**disabled, "terminal": True}
    assert inputs == [disabled, terminal], output
    assert records[0] == disabled, output
    assert records[-2:] == [terminal, {"version": 1, "state": "Stopped"}], output
    connections = [record for record in records if "kind" not in record]
    switches = [record for record in records if record.get("kind") == "switch"]
    assert switches == [{"version": 1, "kind": "switch", "state": "Idle"}], output
    assert len(connections) + len(inputs) + len(switches) == len(records), output
    assert all(set(record) == {"version", "state"} and record["version"] == 1
               for record in connections), output
    return [record["state"] for record in connections]

binary = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else "target/release/peerglide").resolve()
with tempfile.TemporaryDirectory(prefix="peerglide-smoke-") as work:
    root = pathlib.Path(work)
    a, b = root / "a", root / "b"
    for identity in (a, b):
        subprocess.run([str(binary), "init", "--identity", str(identity)], check=True, capture_output=True)
    log = root / "listener.log"
    with log.open("w") as output:
        listener = subprocess.Popen([
            str(binary), "listen", "--identity", str(a),
            "--peer-cert", str(b / "node.crt.der"), "--bind", "127.0.0.1:0",
        ], stdout=output, stderr=subprocess.STDOUT)
        try:
            for _ in range(100):
                found = re.search(r"Listening on (127\.0\.0\.1:\d+)", log.read_text())
                if found:
                    break
                if listener.poll() is not None:
                    raise RuntimeError(log.read_text())
                time.sleep(0.05)
            else:
                raise TimeoutError("listener did not bind")
            probe = subprocess.Popen([
                str(binary), "connect", found[1], "--identity", str(b),
                "--peer-cert", str(a / "node.crt.der"), "--bind", "127.0.0.1:0", "--probe", "--managed",
            ], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
            try:
                assert probe.wait(timeout=10) == 0, probe.stderr.read()
                stdout = probe.stdout.read()
                states = managed_states(stdout)
                assert states == ["Listening", "Authenticated", "ProbePassed", "Stopped"], stdout
            finally:
                if probe.poll() is None:
                    probe.kill()
                    probe.wait()
                probe.stdin.close()
                probe.stdout.close()
                probe.stderr.close()
            assert listener.wait(timeout=10) == 0, log.read_text()
            print("PASS: two executable processes, independent identities, authenticated QUIC probe, clean exit")
        finally:
            if listener.poll() is None:
                listener.terminate()
                listener.wait(timeout=5)

    with log.open("w") as output:
        managed=subprocess.Popen([str(binary),"listen","--identity",str(a),"--peer-cert",str(b/"node.crt.der"),"--bind","127.0.0.1:0","--managed"],stdin=subprocess.PIPE,stdout=output,stderr=subprocess.STDOUT)
        try:
            managed.communicate(input=b"stop\n",timeout=5)
            assert managed.returncode==0,log.read_text()
            states = managed_states(log.read_text())
            assert states[-1] == "Stopped", states
            print("PASS: GUI-managed worker gracefully stops without a connected peer")
        finally:
            if managed.poll() is None:managed.kill();managed.wait()
