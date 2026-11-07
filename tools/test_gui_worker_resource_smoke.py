"""Receipt/entry/parent bounds; never launches a GUI or device session."""
import json
import importlib.util
import os
from pathlib import Path
import subprocess
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

import gui_worker_resource_smoke as check


class ResourceCheck(unittest.TestCase):
    @staticmethod
    def png(width=1280, height=800):
        return (b"\x89PNG\r\n\x1a\n" + (13).to_bytes(4, "big") + b"IHDR"
                + width.to_bytes(4, "big") + height.to_bytes(4, "big") + b"x" * 1100)

    @staticmethod
    def gui():
        return SimpleNamespace(pid=456, poll=lambda: None)

    @staticmethod
    def owner_query(arguments, **options):
        if arguments == ["xdotool", "search", "--onlyvisible", "--pid", "456"]:
            return b"12345\n"
        if arguments == ["xdotool", "getwindowpid", "12345"]:
            return b"456\n"
        raise AssertionError("unexpected owner query")

    def test_existing_screenshot_is_preserved_before_any_external_capture(self):
        with tempfile.TemporaryDirectory() as folder:
            path = Path(folder) / "existing.png"
            path.write_bytes(b"preserve existing bytes")
            with patch.object(check.time, "sleep"), patch.object(check.subprocess, "run") as capture, \
                    patch.object(check.subprocess, "check_output") as query:
                with self.assertRaises(FileExistsError):
                    check.capture_workspace(None, path)
                capture.assert_not_called(); query.assert_not_called()
            self.assertEqual(path.read_bytes(), b"preserve existing bytes")

    def test_existing_compact_screenshot_prevents_all_capture_and_resize(self):
        with tempfile.TemporaryDirectory() as folder:
            path = Path(folder) / "workspace.png"
            compact = Path(folder) / "workspace-compact.png"
            compact.write_bytes(b"preserve compact")
            with patch.object(check.subprocess, "run") as run, \
                    patch.object(check.subprocess, "check_output") as query:
                with self.assertRaises(FileExistsError):
                    check.capture_workspace(self.gui(), path)
                run.assert_not_called(); query.assert_not_called()
            self.assertFalse(path.exists())
            self.assertEqual(compact.read_bytes(), b"preserve compact")

    def test_capture_uses_exact_owned_xid_and_preserves_original_bytes(self):
        data = self.png()
        def capture(arguments, **options):
            self.assertEqual(arguments, ["import", "-window", "12345", "png:-"])
            self.assertTrue(options["check"])
            self.assertEqual(options["timeout"], 10)
            options["stdout"].write(data)
        with tempfile.TemporaryDirectory() as folder, \
                patch.dict(os.environ, {"CI": "true", "DISPLAY": ":99"}), \
                patch.object(check.sys, "platform", "linux"), \
                patch.object(check.subprocess, "check_output", side_effect=self.owner_query) as query, \
                patch.object(check.subprocess, "run", side_effect=capture) as run:
            path = Path(folder) / "app.png"
            self.assertEqual(check.capture_owned_window(self.gui(), path), "12345")
            self.assertEqual(path.read_bytes(), data)
            self.assertEqual(path.stat().st_mode & 0o777, 0o600)
            self.assertEqual(query.call_count, 4, "ownership must be checked before and after capture")
            self.assertEqual(run.call_count, 1)

    def test_missing_ambiguous_or_wrong_owner_never_imports_any_window(self):
        cases = [(b"", None), (b"12345 67890", None), (b"root", None),
                 (b"0", None), (b"9" * 21, None), (b"12345", b"789"),
                 (b"12345", b"456\n789")]
        for windows, owner in cases:
            with self.subTest(windows=windows, owner=owner), tempfile.TemporaryDirectory() as folder, \
                    patch.dict(os.environ, {"CI": "true", "DISPLAY": ":99"}), \
                    patch.object(check.sys, "platform", "linux"), \
                    patch.object(check.subprocess, "check_output", side_effect=[windows, owner]), \
                    patch.object(check.subprocess, "run") as run:
                with self.assertRaises(ValueError):
                    check.capture_owned_window(self.gui(), Path(folder) / "rejected.png")
                run.assert_not_called()

    def test_window_replacement_after_capture_is_rejected_with_evidence_kept(self):
        data = self.png()
        with tempfile.TemporaryDirectory() as folder, \
                patch.dict(os.environ, {"CI": "true", "DISPLAY": ":99"}), \
                patch.object(check.sys, "platform", "linux"), \
                patch.object(check.subprocess, "check_output", side_effect=[b"12345", b"456", b"54321", b"456"]), \
                patch.object(check.subprocess, "run", side_effect=lambda *a, **kw: kw["stdout"].write(data)):
            path = Path(folder) / "changed-window.png"
            with self.assertRaises(ValueError):
                check.capture_owned_window(self.gui(), path)
            self.assertEqual(path.read_bytes(), data)

    def test_import_failure_propagates_and_partial_bytes_are_not_rewritten(self):
        def failed(arguments, **options):
            options["stdout"].write(b"partial evidence")
            raise subprocess.CalledProcessError(1, arguments)
        with tempfile.TemporaryDirectory() as folder, \
                patch.dict(os.environ, {"CI": "true", "DISPLAY": ":99"}), \
                patch.object(check.sys, "platform", "linux"), \
                patch.object(check.subprocess, "check_output", side_effect=self.owner_query), \
                patch.object(check.subprocess, "run", side_effect=failed):
            path = Path(folder) / "failed.png"
            with self.assertRaises(subprocess.CalledProcessError):
                check.capture_owned_window(self.gui(), path)
            self.assertEqual(path.read_bytes(), b"partial evidence")

    def test_png_size_signature_and_dimensions_remain_bounded(self):
        cases = [b"short", b"not-png" * 160, self.png(0, 800), self.png(4097, 800),
                 self.png() + b"x" * (16 * 1024 * 1024)]
        for index, data in enumerate(cases):
            with self.subTest(case=index), tempfile.TemporaryDirectory() as folder, \
                    patch.dict(os.environ, {"CI": "true", "DISPLAY": ":99"}), \
                    patch.object(check.sys, "platform", "linux"), \
                    patch.object(check.subprocess, "check_output", side_effect=self.owner_query), \
                    patch.object(check.subprocess, "run", side_effect=lambda *a, **kw: kw["stdout"].write(data)):
                with self.assertRaises(ValueError):
                    check.capture_owned_window(self.gui(), Path(folder) / "invalid.png")

    def test_workspace_captures_owned_standard_and_compact_then_restores_1280_800(self):
        size = [1280, 800]
        commands = []
        def command(arguments, **options):
            commands.append(arguments)
            if arguments == ["import", "-window", "12345", "png:-"]:
                options["stdout"].write(self.png(*size))
            elif arguments[:4] == ["xdotool", "windowsize", "--sync", "12345"]:
                size[:] = map(int, arguments[4:])
            else:
                raise AssertionError("unexpected screenshot command")
        with tempfile.TemporaryDirectory() as folder, \
                patch.dict(os.environ, {"CI": "true", "DISPLAY": ":99"}), \
                patch.object(check.sys, "platform", "linux"), patch.object(check.time, "sleep"), \
                patch.object(check.subprocess, "check_output", side_effect=self.owner_query), \
                patch.object(check.subprocess, "run", side_effect=command):
            path = Path(folder) / "workspace.png"
            check.capture_workspace(self.gui(), path)
            self.assertEqual(path.read_bytes(), self.png(1280, 800))
            self.assertEqual(path.with_name("workspace-compact.png").read_bytes(), self.png(920, 700))
            self.assertEqual(size, [1280, 800])
            self.assertEqual([c for c in commands if c[0] == "import"],
                             [["import", "-window", "12345", "png:-"]] * 2)

    def test_gui_smoke_import_and_non_ci_entry_never_launch_or_capture(self):
        spec = importlib.util.spec_from_file_location("gui_smoke_import_probe",
                                                     Path(check.__file__).with_name("gui_smoke.py"))
        module = importlib.util.module_from_spec(spec)
        with patch.dict(os.environ, {"CI": "false", "DISPLAY": ":99"}), \
                patch.object(check.sys, "platform", "linux"), \
                patch.object(check.subprocess, "Popen") as launch, \
                patch.object(check.subprocess, "run") as capture, \
                patch.object(check.subprocess, "check_output") as query:
            spec.loader.exec_module(module)
            with self.assertRaises(ValueError):
                module.main()
            launch.assert_not_called(); capture.assert_not_called(); query.assert_not_called()

    def record(self):
        return {"schema_version": 2, "scope": "ci_owned_gui_communication_resources", "state": "connected",
                "worker_pid": 123, "worker_reaped": False, "elapsed_sec": 1.0, "native": False,
                "window_state": "normal_window_requested", "tray_show_received": False, "tray_stop_received": False}

    def test_partial_live_receipt_waits_and_final_truncation_does_not_pass(self):
        with tempfile.TemporaryDirectory() as folder:
            path = Path(folder) / "receipt"
            path.write_bytes(b'{"schema_')
            self.assertEqual(check.records(path), [])
            with self.assertRaises(ValueError):
                check.records(path, final=True)
            item = self.record()
            path.write_text(json.dumps(item) + "\n")
            self.assertEqual(check.records(path, final=True), [item])

    def test_no_future_native_or_private_extra_fields_in_receipt(self):
        with tempfile.TemporaryDirectory() as folder:
            path = Path(folder) / "receipt"
            for key, value in [("schema_version", 3), ("native", True), ("state", "private marker"),
                               ("window_state", "hidden_requested"), ("tray_stop_received", 1),
                               ("key", "private marker"), ("elapsed_sec", float("nan"))]:
                item = self.record(); item[key] = value
                path.write_text(json.dumps(item) + "\n")
                with self.assertRaises(ValueError) as error:
                    check.records(path)
                self.assertNotIn("private marker", str(error.exception))
            path.write_bytes(b"x" * 8193)
            with self.assertRaises(ValueError):
                check.records(path)

    def test_parent_query_is_single_bounded_pid_metadata(self):
        with patch.object(check.subprocess, "check_output", return_value=b" 456\n") as query:
            self.assertEqual(check.parent_pid(123), 456)
            self.assertEqual(query.call_args.args[0], ["ps", "-o", "ppid=", "-p", "123"])
            self.assertEqual(query.call_args.kwargs["timeout"], 2)
        for value in [b"", b"1\n2", b"private/path", b"-1"]:
            with patch.object(check.subprocess, "check_output", return_value=value):
                with self.assertRaises(ValueError):
                    check.parent_pid(123)

    def test_hidden_requires_os_sequence_and_actual_show_stop_receipts(self):
        receipts = []
        for state, window in [("prepared", "normal_window_requested"), ("visible", "appkit_visible"),
                              ("connected", "appkit_hidden"), ("shown", "appkit_visible"),
                              ("stopping", "appkit_visible"), ("completed", "appkit_visible")]:
            item = self.record(); item.update(state=state, window_state=window,
                tray_show_received=state in {"shown", "stopping", "completed"},
                tray_stop_received=state == "completed", worker_reaped=state == "completed")
            receipts.append(item)
        self.assertEqual(check.hidden_visibility_proof(receipts)["sampling"], "appkit_hidden")
        for key in ["tray_show_received", "tray_stop_received", "worker_reaped"]:
            changed = [dict(r) for r in receipts]; changed[-1][key] = False
            with self.assertRaises(ValueError):
                check.hidden_visibility_proof(changed)
        for index in [1, 2, 3]:
            changed = [dict(r) for r in receipts]; changed[index]["window_state"] = "normal_window_requested"
            with self.assertRaises(ValueError):
                check.hidden_visibility_proof(changed)
        with self.assertRaises(ValueError):
            check.hidden_visibility_proof(receipts[1:])

    def test_linux_never_substitutes_a_requested_hidden_flag_for_os_proof(self):
        with tempfile.TemporaryDirectory() as folder:
            report = Path(folder) / "new.json"
            with patch.dict(os.environ, {"CI": "true"}), patch.object(check.sys, "platform", "linux"), \
                    patch.object(check, "run") as run, patch("builtins.print"):
                self.assertEqual(check.main([folder, "--report", str(report), "--tray-hidden"]), 1)
                run.assert_not_called()
            self.assertEqual(json.loads(report.read_text())["reason"], "hidden_check_macos_required")

    def test_non_ci_and_existing_report_refuse_without_starting_gui(self):
        with tempfile.TemporaryDirectory() as folder:
            report = Path(folder) / "new.json"
            with patch.dict(os.environ, {"CI": "false"}), patch.object(check, "run") as run, patch("builtins.print"):
                self.assertEqual(check.main([folder, "--report", str(report)]), 1)
                run.assert_not_called()
            original = report.read_bytes()
            with patch("builtins.print"):
                self.assertEqual(check.main([folder, "--report", str(report)]), 1)
            self.assertEqual(report.read_bytes(), original)
            self.assertFalse(json.loads(original)["passed"])


if __name__ == "__main__":
    unittest.main()
