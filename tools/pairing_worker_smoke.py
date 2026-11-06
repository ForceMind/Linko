#!/usr/bin/env python3
"""Real private-pipe pairing children; random temporary identities, no native IO.

Never print code/IPC records/keys. These are network/process checks, not physical
C06 or persistent enrollment. Failed children remain failures, not mock success.
"""
import hashlib
import json
import pathlib
import queue
import subprocess
import sys
import tempfile
import threading
import time

ROOT = pathlib.Path(__file__).resolve().parent.parent
MAX_LINE = 72 * 1024


class Child:
    def __init__(self, binary, mode, identity, *arguments):
        self.process = subprocess.Popen(
            [str(binary), mode, *arguments, "--identity", str(identity), "--managed"],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        )
        self.events = queue.Queue(maxsize=16)
        self.names = []
        self.errors = bytearray()
        self.forced = False
        self.readers = [threading.Thread(target=self._stdout, daemon=True),
                        threading.Thread(target=self._stderr, daemon=True)]
        for reader in self.readers:
            reader.start()

    def _stdout(self):
        try:
            while True:
                line = self.process.stdout.readline(MAX_LINE + 2)
                if not line:
                    break
                if len(line) > MAX_LINE + 1 or not line.endswith(b"\n"):
                    self.events.put(("invalid", None), timeout=1)
                    break
                try:
                    event = json.loads(line)
                    if event["version"] != 1 or event["kind"] != "pairing_event":
                        raise ValueError()
                    body = event["event"]
                    self.names.append(body["state"])
                    self.events.put(("event", body), timeout=1)
                except (ValueError, KeyError, TypeError, queue.Full):
                    try:
                        self.events.put_nowait(("invalid", None))
                    except queue.Full:
                        pass
                    break
        finally:
            try:
                self.events.put_nowait(("eof", None))
            except queue.Full:
                pass

    def _stderr(self):
        while True:
            chunk = self.process.stderr.read(4096)
            if not chunk:
                return
            # Keep bounded private failure diagnostics, never print them.
            self.errors.extend(chunk[:max(0, 32 * 1024 - len(self.errors))])

    def event(self, wanted, timeout=25):
        deadline = time.monotonic() + timeout
        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise RuntimeError("pairing event deadline exceeded")
            try:
                kind, body = self.events.get(timeout=min(remaining, 1))
            except queue.Empty:
                if self.process.poll() is not None:
                    raise RuntimeError("pairing child exited before expected state") from None
                continue
            if kind != "event":
                raise RuntimeError("invalid or incomplete pairing child stream")
            if body["state"] == wanted:
                return body
            if body["state"] in ("Failed", "Stopped"):
                raise RuntimeError("pairing child reached an unexpected terminal state")

    def command(self, body):
        record = {"version": 1, "kind": "pairing_command", "body": body}
        self.process.stdin.write(json.dumps(record, separators=(",", ":")).encode() + b"\n")
        self.process.stdin.flush()

    def stop(self):
        self.command({"command": "Stop"})

    def wait(self, expected):
        code = self.process.wait(timeout=8)
        for reader in self.readers:
            reader.join(timeout=2)
        if code != expected or any(reader.is_alive() for reader in self.readers):
            raise RuntimeError("pairing child exit/pipe cleanup failed")

    def cleanup(self):
        if self.process.poll() is None:
            try:
                self.stop()
                self.process.wait(timeout=8)
            except (BrokenPipeError, OSError, subprocess.TimeoutExpired):
                # Only this script's non-native, read-only child may be forced.
                self.forced = True
                self.process.terminate()
                try:
                    self.process.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    self.process.kill()
                    self.process.wait(timeout=3)
        for reader in self.readers:
            reader.join(timeout=2)
        for stream in (self.process.stdin, self.process.stdout, self.process.stderr):
            stream.close()


def snapshot(path):
    return {str(file.relative_to(path)): hashlib.sha256(file.read_bytes()).hexdigest()
            for file in path.rglob("*") if file.is_file()}


def identity(binary, path):
    result = subprocess.run([str(binary), "init", "--identity", str(path)],
                            capture_output=True, check=True, timeout=10)
    for line in result.stdout.decode().splitlines():
        if line.startswith("BLAKE3: "):
            return line.removeprefix("BLAKE3: ")
    raise RuntimeError("missing synthetic identity fingerprint")


def main():
    directory = pathlib.Path(sys.argv[1]) if len(sys.argv) > 1 else ROOT / "target/release"
    binary = directory.resolve() / ("peerglide.exe" if sys.platform == "win32" else "peerglide")
    children = []
    codes = []
    with tempfile.TemporaryDirectory(prefix="peerglide-pairing-process-") as temporary:
        root = pathlib.Path(temporary)
        a, b = root / "display", root / "enter"
        fp_a, fp_b = identity(binary, a), identity(binary, b)
        baseline = snapshot(root)

        def offer():
            child = Child(binary, "pair-offer", a)
            children.append(child)
            listening = child.event("Listening")
            address = listening["address"]
            if not address.startswith("127.0.0.1:"):
                raise RuntimeError("pairing smoke must stay on loopback")
            code = listening["code"]
            if len(code) != 8 or not code.isascii() or not code.isdigit():
                raise RuntimeError("invalid pairing code shape")
            codes.append(code)
            return child, address, code

        def connect(address, code=None):
            child = Child(binary, "pair-connect", b, address)
            children.append(child)
            if code is not None:
                child.command({"command": "Code", "code": code})
            return child

        try:
            server, address, code = offer()
            client = connect(address, code)
            x, y = server.event("Authenticated"), client.event("Authenticated")
            if x["context"] != y["context"] or x["local_fingerprint"] != fp_a or y["local_fingerprint"] != fp_b:
                raise RuntimeError("authenticated binding mismatch")
            if bytes(x["certificate"]) != (b / "node.crt.der").read_bytes() or bytes(y["certificate"]) != (a / "node.crt.der").read_bytes():
                raise RuntimeError("authenticated peer certificate mismatch")
            client.stop()
            if client.event("Stopped")["reason"] != "Cancelled":
                raise RuntimeError("stop did not cancel pairing child")
            client.wait(0)
            if server.event("Stopped")["reason"] != "PeerDisconnected":
                raise RuntimeError("peer closure was not observed")
            server.wait(0)

            server, address, code = offer()
            wrong = "87654321" if code == "12345678" else "12345678"
            client = connect(address, wrong)
            if client.event("Failed")["reason"] != "Authentication":
                raise RuntimeError("wrong code did not fail authentication")
            client.wait(1)
            if server.event("AttemptRejected")["remaining"] != 4:
                raise RuntimeError("failed attempt reset display budget")
            server.stop()
            server.event("Stopped")
            server.wait(0)

            server, address, _ = offer()
            client = connect(address)
            client.process.stdin.close()
            if client.event("Stopped")["reason"] != "Cancelled":
                raise RuntimeError("stdin EOF did not stop child")
            client.wait(0)
            server.stop()
            server.event("Stopped")
            server.wait(0)

            client = connect("127.0.0.1:9")
            client.process.stdin.write(b"x" * 257)
            client.process.stdin.flush()
            if client.event("Failed")["reason"] != "InvalidCommand":
                raise RuntimeError("oversized command was not rejected")
            client.wait(1)
            missing = Child(binary, "pair-offer", root / "missing")
            children.append(missing)
            if missing.event("Failed")["reason"] != "Identity":
                raise RuntimeError("missing identity was not rejected")
            missing.wait(1)
            if snapshot(root) != baseline:
                raise RuntimeError("read-only pairing child changed files")
            if any(secret.encode() in child.errors for secret in codes for child in children):
                raise RuntimeError("pairing code leaked into diagnostics")
        finally:
            for child in children:
                child.cleanup()
        if any(child.forced for child in children):
            raise RuntimeError("pairing required forced cleanup")
    print("Pairing worker smoke passed: real private pipes/TCP/PAKE/TLS, stop/peer close, wrong code, EOF, bounds, read-only identity; no native input or enrollment.")


if __name__ == "__main__":
    main()
