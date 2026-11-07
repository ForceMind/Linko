#!/usr/bin/env python3
"""Build a local portable prototype; never publish, install or change permissions.

macOS staging bundles get ad-hoc integrity signatures, not a Developer ID
publisher identity or notarization. No signing credentials, custom requirements,
private entitlements, OS protection changes or TCC edits are used.
"""
import hashlib
import pathlib
import platform
import plistlib
import re
import shutil
import subprocess
import sys
import tempfile
import zipfile

ROOT = pathlib.Path(__file__).resolve().parent.parent
PRODUCT_NAME = "Linko"
APP_BUNDLE_NAME = PRODUCT_NAME + ".app"
APP_DISPLAY_NAME = "Linko · 屏跃"
APP_IDENTIFIER = "com.peerglide.PeerGlide"
WORKER_IDENTIFIER = APP_IDENTIFIER + ".worker"


def bundle_info(version):
    return {"CFBundleName": PRODUCT_NAME, "CFBundleDisplayName": APP_DISPLAY_NAME,
            "CFBundleIdentifier": APP_IDENTIFIER, "CFBundleVersion": version.split("-")[0],
            "CFBundleShortVersionString": version.split("-")[0], "CFBundlePackageType": "APPL",
            "CFBundleExecutable": "peerglide-gui", "NSHighResolutionCapable": True,
            "LSMinimumSystemVersion": "13.0", "NSBonjourServices": ["_peerglide._udp", "_peerglide-pair._tcp"],
            "NSLocalNetworkUsageDescription": "屏跃使用本地网络连接已核验的电脑，并转发用户明确同意的键鼠输入。发现设备不会自动登记信任或授予控制权限。"}


def _display_signature(path, run):
    result = run(["codesign", "--display", "--verbose=4", str(path)],
                 check=True, capture_output=True, text=True)
    return (result.stdout or "") + "\n" + (result.stderr or "")


def _require_identifier(metadata, expected):
    identifiers = re.findall(r"(?m)^Identifier=(.+)$", metadata)
    if identifiers != [expected] or not re.search(r"(?m)^Signature=adhoc$", metadata):
        raise RuntimeError("staging code signature identity is not the expected ad-hoc identity")


def verify_macos_bundle_metadata(app, run):
    """Read actual codesign metadata and the sealed-resource plist on Mac CI.

    This verifies local integrity only. It makes no Accessibility, TCC, launch,
    Gatekeeper, publisher, cross-version identity or notarization promise.
    """
    contents = app / "Contents"
    info = plistlib.loads((contents / "Info.plist").read_bytes())
    if (info.get("CFBundleName") != PRODUCT_NAME
            or info.get("CFBundleDisplayName") != APP_DISPLAY_NAME
            or info.get("CFBundleIdentifier") != APP_IDENTIFIER
            or info.get("CFBundleExecutable") != "peerglide-gui"
            or info.get("CFBundlePackageType") != "APPL"):
        raise RuntimeError("staging app bundle identity metadata mismatch")
    main = _display_signature(app, run)
    worker = _display_signature(contents / "MacOS" / "peerglide", run)
    _require_identifier(main, APP_IDENTIFIER)
    _require_identifier(worker, WORKER_IDENTIFIER)
    bound = re.search(r"(?m)^Info\.plist entries=([0-9]+)$", main)
    if bound is None or int(bound.group(1)) == 0:
        raise RuntimeError("staging app Info.plist is not bound to the code signature")
    if re.search(r"(?m)^Sealed Resources version=2(?: |$)", main) is None:
        raise RuntimeError("staging app has no version-2 resource seal")
    seal = plistlib.loads((contents / "_CodeSignature" / "CodeResources").read_bytes())
    nested = seal.get("files2", {}).get("MacOS/peerglide")
    if (not isinstance(nested, dict) or not isinstance(nested.get("cdhash"), bytes)
            or len(nested["cdhash"]) not in (20, 32)):
        raise RuntimeError("staging helper is not recorded as sealed nested code")


def sign_macos_bundle(app, run):
    """Sign only a newly assembled staging app, inside out, using default DRs."""
    helper = app / "Contents" / "MacOS" / "peerglide"
    run(["codesign", "--force", "--sign", "-", "--identifier", WORKER_IDENTIFIER,
         "--timestamp=none", str(helper)], check=True)
    run(["codesign", "--force", "--sign", "-", "--identifier", APP_IDENTIFIER,
         "--timestamp=none", str(app)], check=True)
    # --deep is verification only: never recursively force-resign nested code.
    run(["codesign", "--verify", "--deep", "--strict", str(app)], check=True)
    verify_macos_bundle_metadata(app, run)


def _stage_bundle(root, bundle, system, version, run):
    bundle.mkdir()
    binary_dir = bundle
    if system == "Darwin":
        contents = bundle / APP_BUNDLE_NAME / "Contents"
        binary_dir = contents / "MacOS"
        binary_dir.mkdir(parents=True)
        (contents / "Info.plist").write_bytes(plistlib.dumps(bundle_info(version)))
    suffix = ".exe" if system == "Windows" else ""
    gui_entry = "peerglide-gui" if system == "Darwin" else ("Linko.exe" if system == "Windows" else "linko")
    # Rename only new staging output. The compiled bin and sibling worker names
    # remain compatible with existing builds and current_exe().with_file_name().
    shutil.copy2(root / "target" / "release" / ("peerglide" + suffix), binary_dir)
    shutil.copy2(root / "target" / "release" / ("peerglide-gui" + suffix), binary_dir / gui_entry)
    for name in ("README.md", "LICENSE", "SECURITY.md"):
        shutil.copy2(root / name, bundle)
    shutil.copytree(root / "docs", bundle / "docs")
    run([sys.executable, str(root / "tools" / "third_party.py"), "--notices", str(bundle / "third-party")],
        cwd=root, check=True)
    signing = ("macOS bundle uses local ad-hoc integrity signatures only; no Developer ID publisher identity or notarization.\n"
               if system == "Darwin" else "Windows/Linux archive has no authenticated publisher signature or notarization.\n")
    launch = ("Open Linko.app; keep its GUI and peerglide worker together inside Contents/MacOS.\n"
              if system == "Darwin" else
              "Launch Linko.exe; keep peerglide.exe in the same directory.\n"
              if system == "Windows" else
              "Launch ./linko; keep ./peerglide in the same directory. Linux supports GUI/communication diagnostics, not native input.\n")
    (bundle / "PROTOTYPE.txt").write_text(
        "Linko experimental prototype with no authenticated publisher identity.\n"
        + launch
        + "Existing configuration, device identities and protocol identifiers are not migrated or replaced by packaging.\n"
        "Physical Windows/macOS dual-machine acceptance remains pending.\n"
        + signing
        + "Windows archive is portable, not an installer.\n"
        "Do not disable OS security protections. Build locally if OS trust policy blocks this prototype.\n"
        "Dependency notices and published source fallbacks are included for review; this is not a completed legal audit.\n",
        encoding="utf-8")
    if system == "Darwin":
        # All app contents exist before signing. Never modify the app after this.
        sign_macos_bundle(bundle / APP_BUNDLE_NAME, run)


def build_package(root=ROOT, system=None, machine=None, run=None):
    """Assemble/sign new temporary staging, verify, then create a new archive."""
    root = pathlib.Path(root)
    system = system or platform.system()
    machine = machine or platform.machine()
    run = run or subprocess.run
    match = re.search(r'(?m)^version = "([^"\n]+)"', (root / "Cargo.toml").read_text())
    if match is None:
        raise ValueError("workspace version is missing")
    version = match.group(1)
    label = f"{PRODUCT_NAME}-{version}-{system.lower()}-{machine.lower()}"
    dist = root / "dist"
    archive = dist / (label + ".zip")
    checksum = archive.with_suffix(".zip.sha256")
    if any(path.exists() or path.is_symlink() for path in (archive, checksum)):
        raise FileExistsError("package output already exists; refusing to replace an artifact")
    dist.mkdir(exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="peerglide-package-") as temporary:
        bundle = pathlib.Path(temporary) / label
        _stage_bundle(root, bundle, system, version, run)
        archive_created = False
        checksum_created = False
        try:
            with zipfile.ZipFile(archive, "x", zipfile.ZIP_DEFLATED, strict_timestamps=False) as zipped:
                archive_created = True
                for file in sorted(bundle.rglob("*")):
                    if file.is_file():
                        zipped.write(file, file.relative_to(bundle.parent))
            digest = hashlib.sha256()
            with archive.open("rb") as source:
                for chunk in iter(lambda: source.read(1024 * 1024), b""):
                    digest.update(chunk)
            with checksum.open("x", encoding="utf-8") as output:
                checksum_created = True
                output.write(f"{digest.hexdigest()}  {archive.name}\n")
        except BaseException:
            # Only outputs created by this invocation are eligible for cleanup.
            if checksum_created:
                checksum.unlink()
            if archive_created:
                archive.unlink()
            raise
    return archive


def main():
    print(build_package())
    return 0


if __name__ == "__main__":
    sys.exit(main())
