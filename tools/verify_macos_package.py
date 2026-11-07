#!/usr/bin/env python3
"""Verify sealed prototype bytes after ZIP extraction; never launches the app."""
import pathlib
import plistlib
import shutil
import subprocess
import sys
import tempfile
import zipfile

from package import APP_BUNDLE_NAME, PRODUCT_NAME, verify_macos_bundle_metadata


def verify(app):
    subprocess.run(["codesign", "--verify", "--deep", "--strict", str(app)], check=True)
    verify_macos_bundle_metadata(app, subprocess.run)


def main():
    if sys.platform != "darwin":
        raise RuntimeError("this verification requires macOS codesign")
    archives = list(pathlib.Path("dist").glob(f"{PRODUCT_NAME}-*-darwin-*.zip"))
    if len(archives) != 1:
        raise RuntimeError("expected one new macOS package")
    with zipfile.ZipFile(archives[0]) as zipped:
        for entry in zipped.infolist():
            path = pathlib.PurePosixPath(entry.filename)
            if path.is_absolute() or ".." in path.parts:
                raise RuntimeError("unsafe package path")
    with tempfile.TemporaryDirectory(prefix="peerglide-signature-roundtrip-") as directory:
        root = pathlib.Path(directory)
        extracted = root / "extracted"
        subprocess.run(["ditto", "-x", "-k", str(archives[0].resolve()), str(extracted)], check=True)
        apps = list(extracted.glob(f"*/{APP_BUNDLE_NAME}"))
        if len(apps) != 1:
            raise RuntimeError("expected one app bundle")
        verify(apps[0])
        for kind in ("plist", "helper"):
            altered = root / kind / APP_BUNDLE_NAME
            shutil.copytree(apps[0], altered)
            if kind == "plist":
                path = altered / "Contents/Info.plist"
                value = plistlib.loads(path.read_bytes())
                value["CFBundleDisplayName"] += " modified fixture"
                path.write_bytes(plistlib.dumps(value))
            else:
                path = altered / "Contents/MacOS/peerglide"
                with path.open("r+b") as file:
                    file.seek(4096)
                    byte = file.read(1)
                    if not byte:
                        raise RuntimeError("helper fixture is too small")
                    file.seek(4096)
                    file.write(bytes([byte[0] ^ 1]))
            result = subprocess.run(["codesign", "--verify", "--deep", "--strict", str(altered)],
                                    capture_output=True)
            if result.returncode == 0:
                raise RuntimeError("modified package was incorrectly accepted")
    print("PASS: extracted macOS bundle identity/seal and modified plist/helper rejection")
    print("This verifies ad-hoc integrity only, not Developer ID, notarization, TCC or physical input.")


if __name__ == "__main__":
    main()
