import zlib
"""Staging/signature contract tests; no codesign, GUI, TCC or real artifacts."""
import hashlib
import importlib.util
import json
from pathlib import Path
import plistlib
import shutil
import struct
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch
import zipfile

import package
import verify_macos_package


MAIN_METADATA = ("Identifier=com.peerglide.PeerGlide\nSignature=adhoc\n"
                 "Info.plist entries=12\nSealed Resources version=2 rules=13 files=1\n")
WORKER_METADATA = "Identifier=com.peerglide.PeerGlide.worker\nSignature=adhoc\nInfo.plist=not bound\n"


class FakeCodesign:
    """Fake byte/metadata effects test the packager, never real signature trust."""
    def __init__(self, fail=None, main=MAIN_METADATA, worker=WORKER_METADATA, seal=True):
        self.calls = []
        self.fail = fail
        self.main = main
        self.worker = worker
        self.seal = seal

    def __call__(self, command, **options):
        self.calls.append((command, options))
        if command[0] != "codesign":
            if self.fail == "notices":
                raise subprocess.CalledProcessError(1, command)
            (Path(command[-1]) / "notice.txt").parent.mkdir()
            (Path(command[-1]) / "notice.txt").write_text("fixture notice")
            return subprocess.CompletedProcess(command, 0)
        path = Path(command[-1])
        if "--sign" in command:
            operation = "app_sign" if path.suffix == ".app" else "helper_sign"
            if self.fail == operation:
                raise subprocess.CalledProcessError(1, command)
            executable = path / "Contents/MacOS/peerglide-gui" if operation == "app_sign" else path
            executable.write_bytes(executable.read_bytes() + b"-fake-signed")
            if operation == "app_sign" and self.seal:
                icon = path / "Contents/Resources/Linko.icns"
                if not icon.is_file():
                    raise AssertionError("icon must exist before the complete bundle is signed")
                seal = path / "Contents/_CodeSignature/CodeResources"
                seal.parent.mkdir()
                nested = {"cdhash": b"f" * 20, "requirement": 'cdhash H"fixture"'}
                if self.seal == "raw_resource":
                    nested = {"hash2": b"not-a-nested-signature"}
                files = {"MacOS/peerglide": nested,
                         "Resources/Linko.icns": {"hash2": hashlib.sha256(icon.read_bytes()).digest()}}
                if self.seal == "missing_icon":
                    files.pop("Resources/Linko.icns")
                elif self.seal == "wrong_icon":
                    files["Resources/Linko.icns"] = {"hash2": b"x" * 32}
                seal.write_bytes(plistlib.dumps({"files2": files}))
        elif "--verify" in command:
            if self.fail == "verify":
                raise subprocess.CalledProcessError(1, command)
        elif "--display" in command:
            if self.fail == "display":
                raise subprocess.CalledProcessError(1, command)
            metadata = self.main if path.suffix == ".app" else self.worker
            return subprocess.CompletedProcess(command, 0, stdout="", stderr=metadata)
        else:
            raise AssertionError("unexpected codesign operation")
        return subprocess.CompletedProcess(command, 0)


def fixture(root):
    (root / "Cargo.toml").write_text('[workspace.package]\nversion = "0.2.3-dev"\n')
    (root / "target/release").mkdir(parents=True)
    for name in ("peerglide", "peerglide-gui", "peerglide.exe", "peerglide-gui.exe"):
        (root / "target/release" / name).write_bytes(b"fixture-" + name.encode())
    for name in ("README.md", "LICENSE", "SECURITY.md"):
        (root / name).write_text("fixture " + name)
    (root / "docs").mkdir()
    (root / "docs/contract.md").write_text("fixture contract")
    brand = root / "apps/peerglide-gui/assets/brand"
    brand.mkdir(parents=True)
    shutil.copyfile(Path(package.__file__).resolve().parent.parent /
                    "apps/peerglide-gui/assets/brand/Linko.icns", brand / "Linko.icns")


class Packaging(unittest.TestCase):

    def test_macos_icon_has_transparent_optical_margin(self):
        root = Path(package.__file__).resolve().parent.parent
        brand = root / "apps/peerglide-gui/assets/brand"
        data = (brand / "Linko.icns").read_bytes()
        offset = 8
        master = None
        while offset < len(data):
            length = int.from_bytes(data[offset + 4:offset + 8], "big")
            if data[offset:offset + 4] == b"ic10":
                master = data[offset + 8:offset + length]
            offset += length
        self.assertIsNotNone(master)
        self.assertEqual(struct.unpack(">II", master[16:24]), (1024, 1024))
        self.assertEqual(master[24:26], bytes([8, 6]))
        compressed = bytearray()
        offset = 8
        while offset < len(master):
            length = int.from_bytes(master[offset:offset + 4], "big")
            if master[offset + 4:offset + 8] == b"IDAT":
                compressed.extend(master[offset + 8:offset + 8 + length])
            offset += length + 12
        raw = zlib.decompress(compressed)
        self.assertEqual(len(raw), 1024 * 4097)
        previous = bytearray(4096)
        bounds = [1024, 1024, -1, -1]
        for y in range(1024):
            kind = raw[y * 4097]
            row = bytearray(raw[y * 4097 + 1:(y + 1) * 4097])
            for i in range(4096):
                a = row[i - 4] if i >= 4 else 0
                b = previous[i]
                c = previous[i - 4] if i >= 4 else 0
                if kind == 0:
                    value = 0
                elif kind == 1:
                    value = a
                elif kind == 2:
                    value = b
                elif kind == 3:
                    value = (a + b) // 2
                else:
                    self.assertEqual(kind, 4)
                    distances = [abs(b - c), abs(a - c), abs(a + b - 2 * c)]
                    value = [a, b, c][distances.index(min(distances))]
                row[i] = (row[i] + value) & 255
            for x in range(1024):
                if row[x * 4 + 3] > 8:
                    bounds = [min(bounds[0], x), min(bounds[1], y),
                              max(bounds[2], x), max(bounds[3], y)]
            previous = row
        for actual, expected in zip(bounds, [100, 100, 923, 923]):
            self.assertLessEqual(abs(actual - expected), 1, bounds)


    def test_committed_native_icons_match_source_and_container_sizes(self):
        root = Path(package.__file__).resolve().parent.parent
        brand = root / "apps/peerglide-gui/assets/brand"
        metadata = json.loads((brand / "icon-source.json").read_text())
        self.assertEqual(hashlib.sha256((root / metadata["source"]).read_bytes()).hexdigest(),
                         metadata["source_sha256"])
        self.assertEqual(hashlib.sha256((root / metadata["macos_source"]).read_bytes()).hexdigest(),
                         metadata["macos_source_sha256"])
        for name, record in metadata["assets"].items():
            data = (brand / name).read_bytes()
            self.assertEqual(len(data), record["bytes"])
            self.assertEqual(hashlib.sha256(data).hexdigest(), record["sha256"])
        for name, size in (("app-icon.png", 1024), ("app-icon-256.png", 256)):
            data = (brand / name).read_bytes()
            self.assertEqual(data[:8], b"\x89PNG\r\n\x1a\n")
            self.assertEqual(struct.unpack(">II", data[16:24]), (size, size))
            self.assertEqual(data[24:26], bytes([8, 6]), "native icon must retain RGBA alpha")
        data = (brand / "Linko.ico").read_bytes()
        reserved, kind, count = struct.unpack("<HHH", data[:6])
        self.assertEqual((reserved, kind, count), (0, 1, 7))
        sizes = []
        offset = 6 + 16 * count
        for index in range(count):
            width, height, _colors, _zero, planes, bits, length, at = struct.unpack_from(
                "<BBBBHHII", data, 6 + 16 * index)
            size = width or 256
            self.assertEqual((height or 256, planes, bits, at), (size, 1, 32, offset))
            png = data[at:at + length]
            self.assertEqual(png[:8], b"\x89PNG\r\n\x1a\n")
            self.assertEqual(struct.unpack(">II", png[16:24]), (size, size))
            sizes.append(size)
            offset += length
        self.assertEqual(sizes, [16, 24, 32, 48, 64, 128, 256])
        self.assertEqual(offset, len(data))
        data = (brand / "Linko.icns").read_bytes()
        self.assertEqual(data[:4], b"icns")
        self.assertEqual(int.from_bytes(data[4:8], "big"), len(data))
        offset, chunks = 8, {}
        while offset < len(data):
            kind = data[offset:offset + 4].decode("ascii")
            length = int.from_bytes(data[offset + 4:offset + 8], "big")
            self.assertGreater(length, 8)
            self.assertLessEqual(offset + length, len(data))
            png = data[offset + 8:offset + length]
            self.assertEqual(png[:8], b"\x89PNG\r\n\x1a\n")
            chunks[kind] = struct.unpack(">II", png[16:24])
            offset += length
        self.assertEqual(offset, len(data))
        self.assertEqual(chunks["ic08"], (256, 256))
        self.assertEqual(chunks["ic09"], (512, 512))
        self.assertEqual(chunks["ic10"], (1024, 1024))

    def test_import_has_no_packaging_subprocess_or_argv_side_effect(self):
        spec = importlib.util.spec_from_file_location("package_import_probe", package.__file__)
        module = importlib.util.module_from_spec(spec)
        with patch.object(package.subprocess, "run") as run, \
                patch.object(package.pathlib.Path, "mkdir") as mkdir, \
                patch.object(package.pathlib.Path, "read_text") as read, \
                patch.object(package.platform, "system") as system, \
                patch.object(sys, "argv", ["package.py", "--not-an-option"]):
            spec.loader.exec_module(module)
        run.assert_not_called()
        mkdir.assert_not_called()
        read.assert_not_called()
        system.assert_not_called()

    def test_macos_signs_staged_helper_then_app_and_verifies_before_archiving(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            fixture(root)
            run = FakeCodesign()
            archive = package.build_package(root, "Darwin", "arm64", run)
            self.assertEqual(archive.name, "Linko-0.2.3-dev-darwin-arm64.zip")
            codesign = [call[0] for call in run.calls if call[0][0] == "codesign"]
            self.assertEqual(["--sign" in command for command in codesign[:3]], [True, True, False])
            self.assertEqual(codesign[0][codesign[0].index("--identifier") + 1], package.WORKER_IDENTIFIER)
            self.assertEqual(codesign[1][codesign[1].index("--identifier") + 1], package.APP_IDENTIFIER)
            self.assertEqual(Path(codesign[0][-1]), Path(codesign[1][-1]) / "Contents/MacOS/peerglide")
            self.assertEqual(codesign[2][1:4], ["--verify", "--deep", "--strict"])
            self.assertNotIn(root / "target/release", Path(codesign[0][-1]).parents)
            for command in codesign[:2]:
                self.assertEqual(command[command.index("--sign") + 1], "-")
                self.assertIn("--timestamp=none", command)
                for prohibited in ("--deep", "--requirements", "-r", "--entitlements"):
                    self.assertNotIn(prohibited, command)
            self.assertTrue(all(options["check"] for _command, options in run.calls))
            self.assertEqual((root / "target/release/peerglide").read_bytes(), b"fixture-peerglide")
            self.assertEqual((root / "target/release/peerglide-gui").read_bytes(), b"fixture-peerglide-gui")
            with zipfile.ZipFile(archive) as zipped:
                label = archive.stem
                app = label + "/Linko.app/Contents/"
                info = plistlib.loads(zipped.read(app + "Info.plist"))
                self.assertEqual(info["CFBundleName"], "Linko")
                self.assertEqual(info["CFBundleDisplayName"], "Linko · 屏跃")
                self.assertEqual(info["CFBundleIdentifier"], package.APP_IDENTIFIER)
                self.assertEqual(info["CFBundleVersion"], "0.2.3")
                self.assertEqual(info["CFBundleExecutable"], "peerglide-gui")
                self.assertEqual(info["CFBundleIconFile"], "Linko.icns")
                self.assertEqual(zipped.read(app + "Resources/Linko.icns"),
                                 (root / "apps/peerglide-gui/assets/brand/Linko.icns").read_bytes())
                self.assertEqual(info["NSBonjourServices"], ["_peerglide._udp", "_peerglide-pair._tcp"])
                self.assertIn(app + "_CodeSignature/CodeResources", zipped.namelist())
                self.assertTrue(zipped.read(app + "MacOS/peerglide").endswith(b"-fake-signed"))
                warning = zipped.read(label + "/PROTOTYPE.txt").decode()
                self.assertIn("local ad-hoc integrity signatures only", warning)
                self.assertIn("no Developer ID publisher identity or notarization", warning)
                self.assertIn("Do not disable OS security protections", warning)
                self.assertIn("Open Linko.app", warning)
                for name in ("README.md", "LICENSE", "SECURITY.md", "docs/contract.md", "third-party/notice.txt"):
                    self.assertIn(label + "/" + name, zipped.namelist())
                self.assertFalse(any("/PeerGlide.app/" in name for name in zipped.namelist()))
                extracted = root / "roundtrip"
                zipped.extractall(extracted)
            before = len(run.calls)
            with patch.object(verify_macos_package.subprocess, "run", new=run):
                verify_macos_package.verify(extracted / label / "Linko.app")
            verification = [command for command, _options in run.calls[before:]]
            self.assertEqual(verification[0][1:4], ["--verify", "--deep", "--strict"])
            self.assertTrue(all("--sign" not in command for command in verification))
            icon = extracted / label / "Linko.app/Contents/Resources/Linko.icns"
            changed = bytearray(icon.read_bytes())
            changed[-1] ^= 1
            icon.write_bytes(changed)
            with patch.object(verify_macos_package.subprocess, "run", new=run):
                with self.assertRaisesRegex(RuntimeError, "icon.*resource seal"):
                    verify_macos_package.verify(extracted / label / "Linko.app")
            self.assertEqual(archive.with_suffix(".zip.sha256").read_text(),
                             f"{hashlib.sha256(archive.read_bytes()).hexdigest()}  {archive.name}\n")
            self.assertFalse(Path(codesign[1][-1]).exists(), "only our temporary staging was signed and removed")

    def test_non_macos_packages_never_call_codesign(self):
        for system in ("Linux", "Windows"):
            with self.subTest(system=system), tempfile.TemporaryDirectory() as folder:
                root = Path(folder)
                fixture(root)
                run = FakeCodesign()
                archive = package.build_package(root, system, "x86_64", run)
                self.assertEqual(archive.name, f"Linko-0.2.3-dev-{system.lower()}-x86_64.zip")
                self.assertFalse(any(command[0] == "codesign" for command, _ in run.calls))
                with zipfile.ZipFile(archive) as zipped:
                    name = "peerglide.exe" if system == "Windows" else "peerglide"
                    self.assertIn(archive.stem + "/" + name, zipped.namelist())
                    gui = "Linko.exe" if system == "Windows" else "linko"
                    suffix = ".exe" if system == "Windows" else ""
                    source = root / "target/release" / ("peerglide-gui" + suffix)
                    self.assertEqual(zipped.read(archive.stem + "/" + gui), source.read_bytes())
                    self.assertEqual(source.read_bytes(), b"fixture-peerglide-gui" + suffix.encode())
                    self.assertNotIn(archive.stem + "/peerglide-gui" + suffix, zipped.namelist())
                    for entry in ("README.md", "LICENSE", "SECURITY.md", "docs/contract.md", "third-party/notice.txt"):
                        self.assertIn(archive.stem + "/" + entry, zipped.namelist())
                    warning = zipped.read(archive.stem + "/PROTOTYPE.txt").decode()
                    self.assertIn("Launch " + (gui if system == "Windows" else "./" + gui), warning)
                    self.assertIn("not migrated or replaced", warning)
                    if system == "Linux":
                        self.assertIn("not native input", warning)
                    self.assertFalse(any("CodeResources" in name for name in zipped.namelist()))

    def test_sign_verify_or_display_failure_blocks_both_archive_and_checksum(self):
        for operation in ("notices", "helper_sign", "app_sign", "verify", "display"):
            with self.subTest(operation=operation), tempfile.TemporaryDirectory() as folder:
                root = Path(folder)
                fixture(root)
                with self.assertRaises(subprocess.CalledProcessError):
                    package.build_package(root, "Darwin", "arm64", FakeCodesign(fail=operation))
                self.assertEqual(list((root / "dist").iterdir()), [])

        for key in ("CFBundleName", "CFBundleDisplayName", "CFBundleIconFile"):
            with self.subTest(plist_key=key), tempfile.TemporaryDirectory() as folder:
                root = Path(folder)
                fixture(root)
                info = package.bundle_info("0.2.3-dev")
                info[key] = "unexpected-brand"
                with patch.object(package, "bundle_info", return_value=info):
                    with self.assertRaises(RuntimeError):
                        package.build_package(root, "Darwin", "arm64", FakeCodesign())
                self.assertEqual(list((root / "dist").iterdir()), [])

    def test_unbound_wrong_identity_or_missing_nested_seal_blocks_archive(self):
        cases = [
            {"main": MAIN_METADATA.replace(package.APP_IDENTIFIER, "peerglide_gui-linker")},
            {"worker": WORKER_METADATA.replace(package.WORKER_IDENTIFIER, package.APP_IDENTIFIER)},
            {"main": MAIN_METADATA.replace("Signature=adhoc", "Signature=Developer ID")},
            {"main": MAIN_METADATA.replace("Info.plist entries=12", "Info.plist=not bound")},
            {"main": MAIN_METADATA.replace("Sealed Resources version=2 rules=13 files=1", "Sealed Resources=none")},
            {"seal": False},
            {"seal": "raw_resource"},
            {"seal": "missing_icon"},
            {"seal": "wrong_icon"},
        ]
        for case in cases:
            with self.subTest(case=case), tempfile.TemporaryDirectory() as folder:
                root = Path(folder)
                fixture(root)
                with self.assertRaises((RuntimeError, FileNotFoundError)):
                    package.build_package(root, "Darwin", "arm64", FakeCodesign(**case))
                self.assertEqual(list((root / "dist").iterdir()), [])

    def test_existing_archive_or_checksum_is_never_replaced_or_resigned(self):
        for suffix in (".zip", ".zip.sha256"):
            with self.subTest(suffix=suffix), tempfile.TemporaryDirectory() as folder:
                root = Path(folder)
                fixture(root)
                (root / "dist").mkdir()
                existing = root / "dist" / ("Linko-0.2.3-dev-darwin-arm64" + suffix)
                existing.write_bytes(b"existing-user-artifact")
                run = FakeCodesign()
                with self.assertRaises(FileExistsError):
                    package.build_package(root, "Darwin", "arm64", run)
                self.assertEqual(existing.read_bytes(), b"existing-user-artifact")
                self.assertEqual(run.calls, [])
                self.assertEqual(len(list((root / "dist").iterdir())), 1)

        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            fixture(root)
            (root / "dist").mkdir()
            legacy = root / "dist/PeerGlide-0.2.3-dev-darwin-arm64.zip"
            legacy.write_bytes(b"existing-user-artifact")
            archive = package.build_package(root, "Darwin", "arm64", FakeCodesign())
            self.assertEqual(legacy.read_bytes(), b"existing-user-artifact")
            self.assertEqual(archive.name, "Linko-0.2.3-dev-darwin-arm64.zip")

    def test_partial_new_archive_is_removed_on_write_failure(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            fixture(root)
            with patch.object(package.zipfile.ZipFile, "write", side_effect=OSError("fixture failure")):
                with self.assertRaises(OSError):
                    package.build_package(root, "Linux", "x86_64", FakeCodesign())
            self.assertEqual(list((root / "dist").iterdir()), [])


if __name__ == "__main__":
    unittest.main()
