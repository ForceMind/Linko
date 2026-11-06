#!/usr/bin/env python3
"""Real v2 pipe/TLS commit exchange, synthetic parent save acknowledgments.

This tests message ordering/retention, NOT a filesystem transaction or GUI
enrollment. Codes, certificates and identity keys are never printed.
"""
import pathlib
import queue
import sys
import tempfile
from pairing_worker_smoke import Child, identity, snapshot, ROOT


def main():
    directory = pathlib.Path(sys.argv[1]) if len(sys.argv) > 1 else ROOT / "target/release"
    binary = directory.resolve() / ("peerglide.exe" if sys.platform == "win32" else "peerglide")
    children = []
    with tempfile.TemporaryDirectory(prefix="peerglide-commit-protocol-") as temporary:
        root = pathlib.Path(temporary)
        aa, bb = root / "a", root / "b"
        identity(binary, aa)
        identity(binary, bb)
        baseline = snapshot(root)

        def pair():
            a = Child(binary, "pair-offer", aa)
            children.append(a)
            offer = a.event("Listening")
            b = Child(binary, "pair-connect", bb, offer["address"])
            children.append(b)
            b.command({"command": "Code", "code": offer["code"]})
            x, y = a.event("Authenticated"), b.event("Authenticated")
            if x["context"] != y["context"]:
                raise RuntimeError("commit protocol context mismatch")
            return a, b, x["context"]

        def command(child, kind, context):
            child.command({"command": kind, "context": context})

        def approve(a, b, context):
            command(a, "Approve", context)
            # One approval cannot ask either parent to save.
            try:
                a.events.get(timeout=0.05)
            except queue.Empty:
                if a.process.poll() is not None:
                    raise RuntimeError("single-approval child exited") from None
            else:
                raise RuntimeError("save became ready before both approvals")
            command(b, "Approve", context)
            if a.event("ReadyToSave")["context"] != context or b.event("ReadyToSave")["context"] != context:
                raise RuntimeError("unbound save request")

        try:
            a, b, context = pair()
            approve(a, b, context)
            command(a, "LocalSaved", context)
            command(b, "LocalSaved", context)
            a.event("PeerSaved")
            # A can finish/exit before B records its confirmation. B's genuine
            # PeerSaved fact must survive the ordinary remote TCP closure.
            command(a, "ReceiptStored", context)
            a.event("Completed")
            a.wait(0)
            b.event("PeerSaved")
            command(b, "ReceiptStored", context)
            b.event("Completed")
            b.wait(0)

            a, b, context = pair()
            approve(a, b, context)
            command(a, "LocalSaved", context)
            # EOF cannot be interpreted as the missing local-save result.
            b.process.stdin.close()
            b.event("Stopped")
            b.wait(0)
            if a.event("Failed")["reason"] != "CommitUnknown":
                raise RuntimeError("lost local-save notification did not retain commit uncertainty")
            a.wait(1)
            if "PeerSaved" in a.names or "Completed" in a.names + b.names:
                raise RuntimeError("lost LocalSaved notification fabricated completion")

            a, b, context = pair()
            approve(a, b, context)
            command(a, "LocalSaved", context)
            command(b, "LocalSaved", context)
            a.event("PeerSaved")
            b.event("PeerSaved")
            command(a, "ReceiptStored", context)
            a.event("Completed")
            a.wait(0)
            b.process.stdin.close()
            b.event("Stopped")
            b.wait(0)
            if "Completed" in b.names:
                raise RuntimeError("missing ReceiptStored notification was accepted")

            a, b, context = pair()
            wrong = list(context)
            wrong[0] ^= 1
            command(a, "Approve", wrong)
            if a.event("Failed")["reason"] != "Protocol":
                raise RuntimeError("changed commit context was not rejected")
            a.wait(1)
            b.event("Stopped")
            b.wait(0)
            if snapshot(root) != baseline:
                raise RuntimeError("commit protocol worker wrote files")
        finally:
            for child in children:
                child.cleanup()
        if any(child.forced for child in children):
            raise RuntimeError("commit exchange required forced cleanup")
    print("Pairing v2 commit exchange passed: real pipes/TLS, approval barrier, early peer facts, lost notifications, wrong context; synthetic parent save acks only, no enrollment/native input.")


if __name__ == "__main__":
    main()
