#!/usr/bin/env python3
"""Actual AppKit terminate -> owned Quit in an empty, non-native GUI subprocess.

No Dock interaction, OS input, network, credentials or security changes. Exit 0
without the bounded positive receipt is a failure, not proof of recovery.
"""
import json
import pathlib
import subprocess
import sys
import tempfile


def main():
    if sys.platform != "darwin":
        raise SystemExit("macOS only; no platform acceptance performed")
    binary_dir = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else "target/release").resolve()
    gui = binary_dir / "peerglide-gui"
    with tempfile.TemporaryDirectory(prefix="peerglide-system-quit-") as work:
        root = pathlib.Path(work)
        profile = root / "profile"
        profile.mkdir()
        report = root / "receipt.json"
        with (root / "diagnostic.log").open("xb") as log:
            process = subprocess.Popen([str(gui), "--data-dir", str(profile),
                "--macos-quit-check-report", str(report)],
                stdin=subprocess.DEVNULL, stdout=log, stderr=subprocess.STDOUT)
            try:
                exit_code = process.wait(timeout=20)
                if exit_code != 0:
                    # The empty profile contains no identity/keys or input. Keep
                    # a bounded own-process diagnostic instead of losing it with
                    # TemporaryDirectory cleanup; do not print host path details.
                    log.flush()
                    with (root / "diagnostic.log").open("rb") as source:
                        source.seek(0, 2)
                        source.seek(max(0, source.tell() - 4096))
                        tail = source.read(4096).decode("utf-8", errors="replace")
                    tail = tail.replace(str(binary_dir), "[binary]").replace(str(root), "[isolated-check]").replace(str(pathlib.Path.home()), "[home]")
                    raise RuntimeError(f"GUI exit {exit_code}; native terminate check failed\n{tail}")
                if not report.is_file() or not 0 < report.stat().st_size <= 4096:
                    raise AssertionError("GUI exited without a complete bounded receipt")
                with report.open("rb") as source:
                    data = source.read(4097)
                assert len(data) <= 4096
                value = json.loads(data)
                assert value["schema_version"] == 1 and value["scope"] == "isolated_real_appkit_terminate_to_owned_quit"
                for key in ("success", "terminate_returned", "same_mailbox_quit_received", "no_worker_or_input", "exit_owner_healthy", "closing_in_gui_logic"):
                    assert value[key] is True, (key, value)
                assert not (profile / "identity").exists() and not (profile / "trusted-peer.crt.der").exists()
                print("PASS: real AppKit terminate returned, owned Quit reached GUI logic, isolated GUI closed; no native input")
                print(json.dumps(value, sort_keys=True))
            finally:
                if process.poll() is None:
                    # This exact child has a fresh empty profile and cannot start
                    # native/network work. Never apply this to an actual KVM run.
                    process.terminate()
                    try:
                        process.wait(timeout=3)
                    except subprocess.TimeoutExpired:
                        process.kill()
                        process.wait(timeout=3)


if __name__ == "__main__":
    main()
