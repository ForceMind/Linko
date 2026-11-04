#!/usr/bin/env python3
"""Build an unsigned portable prototype bundle, never publish or deploy it."""
import hashlib
import pathlib
import platform
import plistlib
import shutil
import subprocess
import tempfile
import zipfile

root = pathlib.Path(__file__).resolve().parent.parent
dist = root / "dist"
dist.mkdir(exist_ok=True)
system = platform.system()
suffix = ".exe" if system == "Windows" else ""
label = f"PeerGlide-0.1.0-dev-{system.lower()}-{platform.machine().lower()}"
with tempfile.TemporaryDirectory(prefix="peerglide-package-") as temporary:
    bundle = pathlib.Path(temporary) / label
    bundle.mkdir()
    binary_dir = bundle
    if system == "Darwin":
        contents = bundle / "PeerGlide.app" / "Contents"
        binary_dir = contents / "MacOS"
        binary_dir.mkdir(parents=True)
        plist = {"CFBundleName": "PeerGlide", "CFBundleDisplayName": "屏跃",
                 "CFBundleIdentifier": "com.peerglide.PeerGlide", "CFBundleVersion": "1",
                 "CFBundleShortVersionString": "0.1.0", "CFBundlePackageType": "APPL",
                 "CFBundleExecutable": "peerglide-gui", "NSHighResolutionCapable": True,
                 "LSMinimumSystemVersion": "13.0",
                 "NSBonjourServices": ["_peerglide._udp"],
                 "NSLocalNetworkUsageDescription": "PeerGlide discovers computers on your local network. Discovery never grants control."}
        (contents / "Info.plist").write_bytes(plistlib.dumps(plist))
    for name in ("peerglide", "peerglide-gui"):
        shutil.copy2(root / "target" / "release" / (name + suffix), binary_dir)
    for name in ("README.md", "LICENSE", "SECURITY.md"):
        shutil.copy2(root / name, bundle)
    shutil.copytree(root / "docs", bundle / "docs")
    subprocess.run(["python", str(root / "tools" / "third_party.py"), "--notices", str(bundle / "third-party")], cwd=root, check=True)
    (bundle / "PROTOTYPE.txt").write_text(
        "Experimental unsigned prototype. Launch peerglide-gui with the CLI beside it.\n"
        "Physical Windows/macOS dual-machine acceptance remains pending. No clipboard/files or tray yet.\n"
        "macOS bundle is neither signed nor notarized. Windows archive is portable, not an installer.\n"
        "Do not disable OS security protections. Build locally if OS trust policy blocks unsigned software.\n"
        "Dependency notices and published source fallbacks are included for review; this is not a completed legal audit.\n",
        encoding="utf-8")
    archive = dist / (label + ".zip")
    with zipfile.ZipFile(archive, "w", zipfile.ZIP_DEFLATED, strict_timestamps=False) as zipped:
        for file in sorted(bundle.rglob("*")):
            if file.is_file():
                zipped.write(file, file.relative_to(bundle.parent))
    digest = hashlib.sha256(archive.read_bytes()).hexdigest()
    archive.with_suffix(".zip.sha256").write_text(f"{digest}  {archive.name}\n", encoding="utf-8")
    print(archive)
