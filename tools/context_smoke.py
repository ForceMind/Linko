#!/usr/bin/env python3
"""Read-only native context diagnostics; no capture, injection or permission prompt.

Only schema/status assertions are printed. Application IDs stay in memory and are
never uploaded/logged; lack of a runner desktop is a real unavailable result.
"""
import json
import pathlib
import subprocess
import sys
import tempfile

binary = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else "target/release/peerglide").resolve()
statuses = {"unsupported", "no_foreground", "no_focus", "changed_during_read", "permission_denied", "identifier_unavailable", "identifier_too_long", "query_failed", "available"}
permission_statuses = {"granted", "denied", "not_required", "unknown", "unsupported"}
with tempfile.TemporaryDirectory(prefix="peerglide-context-") as temporary:
    for include in (False, True):
        command = [str(binary), "diagnose-context"]
        if include:
            command.append("--include-application-id")
        result = subprocess.run(command, cwd=temporary, capture_output=True, text=True, timeout=15)
        assert result.returncode == 0, "context diagnostic failed; payload intentionally omitted"
        report = json.loads(result.stdout)
        assert report["schema_version"] == 1
        assert report["action_execution_supported"] is False
        assert report["native_capture_started"] is False
        assert report["observation_scope"] == "sampled_no_notifications"
        assert report["foreground_status"] in statuses
        assert report["focus_status"] in {"available", "unavailable", "permission_denied", "unsupported"}
        if report["foreground_status"] == "available":
            assert report["recheck_status"] in statuses | {"matched_at_recheck", "changed"}
        else:
            assert report["focus_status"] == "unavailable"
            assert report["recheck_status"] == "unavailable"
        expected_fields = {"schema_version", "platform", "observation_scope", "action_execution_supported", "native_capture_started", "permissions", "foreground_status", "focus_status", "recheck_status"}
        if include and report["foreground_status"] == "available":
            expected_fields.add("application_id")
        assert set(report) == expected_fields, "diagnostic fields must remain explicitly allowlisted"
        assert set(report["permissions"]) == {"accessibility", "input_monitoring", "input_desktop", "secure_input"}
        for key in ("accessibility", "input_monitoring", "input_desktop"):
            assert report["permissions"][key] in permission_statuses
        assert report["permissions"]["secure_input"] in {"active", "inactive", "unknown", "unsupported"}
        assert not {"pid", "hwnd", "title", "target_token", "window", "private_key"} & set(report)
        if include and report["foreground_status"] == "available":
            value = report["application_id"]
            assert 0 < len(value.encode("utf-8")) <= 256 and value.strip() == value
            assert value.startswith("mac-bundle:" if report["platform"] == "macos" else "win-exe:")
        else:
            assert "application_id" not in report
        if sys.platform.startswith("linux"):
            assert report["foreground_status"] == "unsupported"
        assert not list(pathlib.Path(temporary).iterdir()), "diagnostics must not create identity/config/storage"
    observer = subprocess.run([str(binary), "diagnose-context", "--observe-ms", "50"], cwd=temporary, capture_output=True, text=True, timeout=15)
    assert observer.returncode == 0, "observer diagnostic failed; payload intentionally omitted"
    observed = json.loads(observer.stdout)
    assert set(observed) == {"schema_version", "platform", "observation_scope", "action_execution_supported", "native_capture_started", "permissions", "foreground_status", "focus_status", "recheck_status", "observer"}
    assert observed["observation_scope"] == "sampled_with_observer_attempt"
    assert "application_id" not in observed
    assert observed["action_execution_supported"] is False and observed["native_capture_started"] is False
    summary = observed["observer"]
    assert set(summary) == {"requested_ms", "status", "coalesced_invalidations", "context_minted", "delivery_verified"}
    assert summary["status"] != "cleanup_failed", "native observer cleanup failure is a real check failure"
    assert summary["requested_ms"] == 50 and summary["context_minted"] is False and summary["delivery_verified"] is False
    assert summary["status"] in (statuses - {"available"}) | {"ready", "not_ready", "busy", "wrong_thread", "native_unavailable", "backlog", "closed", "entropy_unavailable", "counter_exhausted", "clock_regressed", "stale"}
    if sys.platform.startswith("linux"):
        assert summary["status"] == "unsupported"
    assert not list(pathlib.Path(temporary).iterdir())
    rejected = subprocess.run([str(binary), "diagnose-context", "--native"], cwd=temporary, capture_output=True, timeout=10)
    assert rejected.returncode != 0, "diagnostics must not accept native input flags"
print("PASS: readonly native context/status schema, opt-in identifiers, no files or input authority")
