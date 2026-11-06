#!/usr/bin/env python3
"""Run actual GUI-model enrollment against the exact release CLI, no display.

The test owns two fresh temporary profiles; no native flags or real user profile.
"""
import os
import pathlib
import re
import subprocess
import sys

root = pathlib.Path(__file__).resolve().parent.parent
directory = pathlib.Path(sys.argv[1]) if len(sys.argv) > 1 else root / "target/release"
worker = directory.resolve() / ("peerglide.exe" if sys.platform == "win32" else "peerglide")
if not worker.is_file():
    raise SystemExit("matching pairing CLI is required")
env = dict(os.environ)
env["PEERGLIDE_PAIRING_TEST_WORKER"] = str(worker)
result = subprocess.run(["cargo", "test", "-p", "peerglide-gui", "--locked",
                "pairing_panel::tests::real_gui_model_pairing_commits_pending_then_confirmed_without_native",
                "--", "--exact", "--ignored"], cwd=root, env=env, capture_output=True, text=True, timeout=120)
print(result.stdout, end="")
print(result.stderr, end="", file=sys.stderr)
output = re.sub(r"\x1b\[[0-9;]*m", "", result.stdout)
if (result.returncode != 0 or "1 passed; 0 failed; 0 ignored" not in output
        or "test pairing_panel::tests::real_gui_model_pairing_commits_pending_then_confirmed_without_native ... ok" not in output):
    raise SystemExit("actual GUI-model enrollment test did not run and pass exactly once")
