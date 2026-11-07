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
        return {"schema_version": 1, "scope": "ci_owned_gui_communication_resources", "state": "connected",
                "worker_pid": 123, "worker_reaped": False, "elapsed_sec": 1.0, "native": False}

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
            for key, value in [("schema_version", 2), ("native", True), ("state", "private marker"),
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
