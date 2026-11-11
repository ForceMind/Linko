#!/usr/bin/env python3
"""Build a local portable prototype; never publish, install or change permissions.

macOS staging bundles get ad-hoc integrity signatures, not a Developer ID
publisher identity or notarization. No signing credentials, custom requirements,
private entitlements, OS protection changes or TCC edits are used.
"""
import hashlib
import json
import os
import pathlib
import platform
import plistlib
import re
import shutil
import subprocess
import sys
import tempfile
import zipfile

import build_version

ROOT = pathlib.Path(__file__).resolve().parent.parent
PRODUCT_NAME = "Linko"
APP_BUNDLE_NAME = PRODUCT_NAME + ".app"
APP_DISPLAY_NAME = "Linko · 屏跃"
APP_ICON_FILE = "Linko.icns"
APP_IDENTIFIER = "com.peerglide.PeerGlide"
WORKER_IDENTIFIER = APP_IDENTIFIER + ".worker"
SOURCE_MANIFEST = "BUILD_SOURCE.json"
_OBJECT_ID = re.compile(r"[0-9a-f]{40}")


def bundle_versions(version):
    """Date-time builds give (YYYY.M.D, YYYYMMDD.HMM); other strings keep the legacy form."""
    try:
        return build_version.bundle_versions_from_display(version)
    except ValueError:
        return version.split("-")[0], version.split("-")[0]


def resolve_version(root):
    """LINKO_VERSION wins (validated); else git commit time; else the Cargo.toml fallback."""
    configured = os.environ.get("LINKO_VERSION")
    if configured:
        try:
            return build_version.validate_display_version(configured)
        except ValueError:
            raise ValueError("LINKO_VERSION must look like YYYY.M.D-HHMM") from None
    try:
        return build_version.display_version(build_version.commit_timestamp(root))
    except (OSError, subprocess.CalledProcessError, ValueError):
        pass
    match = re.search(r'(?m)^version = "([^"\n]+)"', (root / "Cargo.toml").read_text())
    if match is None:
        raise ValueError("workspace version is missing")
    return match.group(1)


def build_source(root, version, system, machine):
    """Record the Git objects used by this build, including a PR merge checkout."""
    review = os.environ.get("LINKO_REVIEW_HEAD")
    github = os.environ.get("GITHUB_SHA") if review else None
    if review is not None and not _OBJECT_ID.fullmatch(review):
        raise ValueError("invalid review commit for package")
    manifest = {
        "schema_version": 1,
        "product": PRODUCT_NAME,
        "application_id": APP_IDENTIFIER,
        "build_version": version,
        "system": system.lower(),
        "machine": machine.lower(),
        "checkout_commit": None,
        "checkout_tree": None,
        "review_commit": review,
    }
    try:
        result = subprocess.run(
            ["git", "-C", str(root), "show", "-s", "--format=%H%n%T%n%P", "HEAD"],
            check=True, capture_output=True, text=True,
        )
    except (OSError, subprocess.CalledProcessError):
        if review is not None:
            raise ValueError("review package requires a Git checkout") from None
        return manifest
    lines = result.stdout.splitlines()
    if len(lines) != 3:
        raise ValueError("invalid package source metadata")
    commit, tree, parents = lines
    if not all(_OBJECT_ID.fullmatch(item) for item in [commit, tree, *parents.split()]):
        raise ValueError("invalid package source object")
    if github is not None and github != commit:
        raise ValueError("runner checkout differs from GitHub source commit")
    if review is not None and review not in [commit, *parents.split()]:
        raise ValueError("review commit does not match the runner checkout")
    manifest["checkout_commit"] = commit
    manifest["checkout_tree"] = tree
    return manifest


def bundle_info(version):
    short_version, build_number = bundle_versions(version)
    return {"CFBundleName": PRODUCT_NAME, "CFBundleDisplayName": APP_DISPLAY_NAME,
            "CFBundleIdentifier": APP_IDENTIFIER, "CFBundleVersion": build_number,
            "CFBundleShortVersionString": short_version, "CFBundlePackageType": "APPL",
            "CFBundleExecutable": "peerglide-gui", "NSHighResolutionCapable": True,
            "CFBundleIconFile": APP_ICON_FILE,
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
            or info.get("CFBundleIconFile") != APP_ICON_FILE
            or info.get("CFBundlePackageType") != "APPL"):
        raise RuntimeError("staging app bundle identity metadata mismatch")
    icon = (contents / "Resources" / APP_ICON_FILE).read_bytes()
    if icon[:4] != b"icns" or len(icon) <= 8 or int.from_bytes(icon[4:8], "big") != len(icon):
        raise RuntimeError("staging app icon is not a complete ICNS resource")
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
    icon_seal = seal.get("files2", {}).get("Resources/" + APP_ICON_FILE)
    if not isinstance(icon_seal, dict) or icon_seal.get("hash2") != hashlib.sha256(icon).digest():
        raise RuntimeError("staging app icon is missing from the resource seal or changed")


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


def _stage_bundle(root, bundle, system, version, source, run):
    bundle.mkdir()
    (bundle / SOURCE_MANIFEST).write_text(
        json.dumps(source, ensure_ascii=True, sort_keys=True, indent=2) + "\n",
        encoding="utf-8",
    )
    binary_dir = bundle
    if system == "Darwin":
        contents = bundle / APP_BUNDLE_NAME / "Contents"
        binary_dir = contents / "MacOS"
        binary_dir.mkdir(parents=True)
        (contents / "Info.plist").write_bytes(plistlib.dumps(bundle_info(version)))
        resources = contents / "Resources"
        resources.mkdir()
        shutil.copy2(root / "apps/peerglide-gui/assets/brand" / APP_ICON_FILE,
                     resources / APP_ICON_FILE)
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
    version = resolve_version(root)
    source = build_source(root, version, system, machine)
    label = f"{PRODUCT_NAME}-{version}-{system.lower()}-{machine.lower()}"
    dist = root / "dist"
    archive = dist / (label + ".zip")
    checksum = archive.with_suffix(".zip.sha256")
    if any(path.exists() or path.is_symlink() for path in (archive, checksum)):
        raise FileExistsError("package output already exists; refusing to replace an artifact")
    dist.mkdir(exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="peerglide-package-") as temporary:
        bundle = pathlib.Path(temporary) / label
        _stage_bundle(root, bundle, system, version, source, run)
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
