"""Receipt/entry/parent bounds; never launches a GUI or device session."""
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import gui_worker_resource_smoke as check


class ResourceCheck(unittest.TestCase):
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

    @patch.object(check.sys, "platform", "linux")
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
            self.assertEqual(json.loads(report.read_text())["reason"], "hidden_check_platform_unsupported")

    def test_windows_without_interactive_station_fails_before_identity_or_gui(self):
        import win_process_resources
        with tempfile.TemporaryDirectory() as folder:
            with patch.object(check.sys, "platform", "win32"), \
                    patch.object(win_process_resources, "environment", return_value={"interactive_station": False, "shell_tray_present": False}), \
                    patch.object(check.subprocess, "run") as init, patch.object(check.subprocess, "Popen") as spawn:
                value = check.run(Path(folder), 17, check.metrics.StopFlag(), True)
                self.assertFalse(value["passed"])
                self.assertEqual(value["reason"], "windows_environment_failed")
                init.assert_not_called(); spawn.assert_not_called()

    def test_win32_visibility_sequence_is_distinct_from_appkit_or_requested(self):
        values = []
        for state, window in [("prepared", "normal_window_requested"), ("visible", "win32_visible"),
                              ("connected", "win32_hidden"), ("shown", "win32_visible"),
                              ("stopping", "win32_visible"), ("completed", "win32_visible")]:
            item = self.record(); item.update(state=state, window_state=window,
                tray_show_received=state in {"shown", "stopping", "completed"},
                tray_stop_received=state == "completed", worker_reaped=state == "completed")
            values.append(item)
        self.assertEqual(check.hidden_visibility_proof(values)["sampling"], "win32_hidden")
        values[2]["window_state"] = "appkit_hidden"
        with self.assertRaises(ValueError):
            check.hidden_visibility_proof(values)

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
