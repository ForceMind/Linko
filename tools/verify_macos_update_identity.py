#!/usr/bin/env python3
"""Compare macOS code requirements across two full bundles without launching them."""
import argparse
from pathlib import Path
import re
import subprocess


def designated_requirement(binary):
    result = subprocess.run(
        ["codesign", "--display", "--requirements", "-", str(binary)],
        capture_output=True, text=True, check=True,
    )
    requirements = re.findall(
        r"(?m)^(?:#\s*)?designated => (.+)$", result.stdout + result.stderr
    )
    if len(requirements) != 1:
        raise ValueError("expected exactly one designated requirement")
    return requirements[0]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("previous", type=Path)
    parser.add_argument("candidate", type=Path)
    args = parser.parse_args()
    previous, candidate = args.previous.resolve(), args.candidate.resolve()
    for app in (previous, candidate):
        subprocess.run(["codesign", "--verify", "--deep", "--strict", str(app)], check=True)
    compatible = True
    for label, relative in (("App", Path(".")), ("worker", Path("Contents/MacOS/peerglide"))):
        old = previous / relative
        requirement = designated_requirement(old)
        # Verify the extracted requirement on the original first. A malformed
        # requirement must not be mistaken for an incompatible upgrade.
        command = ["codesign", "--verify", "--strict", "-R=" + requirement]
        subprocess.run([*command, str(old)], check=True)
        result = subprocess.run([*command, str(candidate / relative)], capture_output=True)
        print(f"{label}: {'compatible' if result.returncode == 0 else 'incompatible'}")
        compatible &= result.returncode == 0
    return 0 if compatible else 1


if __name__ == "__main__":
    raise SystemExit(main())
