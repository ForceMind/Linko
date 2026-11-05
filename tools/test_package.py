"""Staging/signature contract tests; no codesign, GUI, TCC or real artifacts."""
import importlib.util
from pathlib import Path
import plistlib
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch
import zipfile

import package


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
                seal = path / "Contents/_CodeSignature/CodeResources"
                seal.parent.mkdir()
                nested = {"cdhash": b"f" * 20, "requirement": 'cdhash H"fixture"'}
                if self.seal == "raw_resource":
                    nested = {"hash2": b"not-a-nested-signature"}
                seal.write_bytes(plistlib.dumps({"files2": {"MacOS/peerglide": nested}}))
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


class Packaging(unittest.TestCase):
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
                app = label + "/PeerGlide.app/Contents/"
                info = plistlib.loads(zipped.read(app + "Info.plist"))
                self.assertEqual(info["CFBundleIdentifier"], package.APP_IDENTIFIER)
                self.assertEqual(info["CFBundleVersion"], "0.2.3")
                self.assertEqual(info["CFBundleExecutable"], "peerglide-gui")
                self.assertIn(app + "_CodeSignature/CodeResources", zipped.namelist())
                self.assertTrue(zipped.read(app + "MacOS/peerglide").endswith(b"-fake-signed"))
                warning = zipped.read(label + "/PROTOTYPE.txt").decode()
                self.assertIn("local ad-hoc integrity signatures only", warning)
                self.assertIn("no Developer ID publisher identity or notarization", warning)
                self.assertIn("Do not disable OS security protections", warning)
            self.assertTrue(archive.with_suffix(".zip.sha256").is_file())
            self.assertFalse(Path(codesign[1][-1]).exists(), "only our temporary staging was signed and removed")

    def test_non_macos_packages_never_call_codesign(self):
        for system in ("Linux", "Windows"):
            with self.subTest(system=system), tempfile.TemporaryDirectory() as folder:
                root = Path(folder)
                fixture(root)
                run = FakeCodesign()
                archive = package.build_package(root, system, "x86_64", run)
                self.assertFalse(any(command[0] == "codesign" for command, _ in run.calls))
                with zipfile.ZipFile(archive) as zipped:
                    name = "peerglide.exe" if system == "Windows" else "peerglide"
                    self.assertIn(archive.stem + "/" + name, zipped.namelist())
                    self.assertFalse(any("CodeResources" in name for name in zipped.namelist()))

    def test_sign_verify_or_display_failure_blocks_both_archive_and_checksum(self):
        for operation in ("notices", "helper_sign", "app_sign", "verify", "display"):
            with self.subTest(operation=operation), tempfile.TemporaryDirectory() as folder:
                root = Path(folder)
                fixture(root)
                with self.assertRaises(subprocess.CalledProcessError):
                    package.build_package(root, "Darwin", "arm64", FakeCodesign(fail=operation))
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
                existing = root / "dist" / ("PeerGlide-0.2.3-dev-darwin-arm64" + suffix)
                existing.write_bytes(b"existing-user-artifact")
                run = FakeCodesign()
                with self.assertRaises(FileExistsError):
                    package.build_package(root, "Darwin", "arm64", run)
                self.assertEqual(existing.read_bytes(), b"existing-user-artifact")
                self.assertEqual(run.calls, [])
                self.assertEqual(len(list((root / "dist").iterdir())), 1)

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
