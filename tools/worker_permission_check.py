#!/usr/bin/env python3
"""Run the actual GUI diagnostic owner with the matching CLI, no input or GUI."""
import os
import pathlib
import re
import subprocess
import sys

root = pathlib.Path(__file__).resolve().parent.parent
directory = pathlib.Path(sys.argv[1]) if len(sys.argv) > 1 else root / "target/release"
worker = directory.resolve() / ("peerglide.exe" if sys.platform == "win32" else "peerglide")
if not worker.is_file():
    raise SystemExit("matching diagnostic CLI is required")
env = dict(os.environ)
env["PEERGLIDE_READINESS_TEST_WORKER"] = str(worker)
test = "worker_permissions::tests::real_worker_readiness_remains_readonly_and_requires_separate_consent"
result = subprocess.run(["cargo", "test", "-p", "peerglide-gui", "--locked", test,
                         "--", "--exact", "--ignored"], cwd=root, env=env,
                        capture_output=True, text=True, timeout=60)
print(result.stdout, end="")
print(result.stderr, end="", file=sys.stderr)
output = re.sub(r"\x1b\[[0-9;]*m", "", result.stdout)
if (result.returncode != 0 or "1 passed; 0 failed; 0 ignored" not in output
        or "test " + test + " ... ok" not in output):
    raise SystemExit("actual worker readiness test did not run and pass exactly once")
