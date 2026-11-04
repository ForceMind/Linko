#!/usr/bin/env python3
"""Local Codex preflight. No installs, native capture, privilege changes or uploads.
Writes a new report under --output-root (default: target/local-validation);
Cargo also writes its normal build/cache outputs. Python bytecode is disabled.
Passing this script never implies real-desktop acceptance.
"""
import argparse
import datetime
import json
import os
import pathlib
import platform
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent


def inventory(command):
    try:
        result = subprocess.run(command, cwd=ROOT, capture_output=True, text=True, timeout=15)
        return result.stdout.strip() if result.returncode == 0 else "unavailable"
    except (OSError, subprocess.TimeoutExpired):
        return "unavailable"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--inventory-only", action="store_true", help="Collect versions only; all checks stay not_run")
    parser.add_argument("--output-root", type=pathlib.Path, default=ROOT / "target/local-validation")
    parser.add_argument("--audit", action="store_true", help="Also require already installed cargo-audit; deny advisory warnings, never install it")
    args = parser.parse_args()
    now = datetime.datetime.now(datetime.timezone.utc)
    output = args.output_root / f"preflight-{now.strftime('%Y%m%dT%H%M%S%fZ')}-{os.getpid()}"
    output.mkdir(parents=True, exist_ok=False)
    dirty = inventory(["git", "status", "--porcelain"])
    report = {
        "schema_version": 1, "timestamp_utc": now.isoformat(),
        "revision": inventory(["git", "rev-parse", "HEAD"]),
        "dirty_entries": None if dirty == "unavailable" else len(dirty.splitlines()),
        "system": platform.system(), "os_release": platform.release(), "architecture": platform.machine(),
        "rustc": inventory(["rustc", "-vV"]), "cargo": inventory(["cargo", "--version"]),
        "dependency_audit_requested": args.audit,
        "native_desktop_acceptance": "not_run", "native_gui_interaction": "not_run",
        "bluetooth_usb_dpi_permissions": "not_run", "checks": [], "status": "inventory_only",
    }
    binary = ROOT / "target/debug" / ("peerglide.exe" if os.name == "nt" else "peerglide")
    checks = [
        ("fmt", ["cargo", "fmt", "--all", "--", "--check"]),
        ("clippy", ["cargo", "clippy", "--workspace", "--all-targets", "--locked", "--", "-D", "warnings"]),
        ("tests", ["cargo", "test", "--workspace", "--locked"]),
        ("notice_tests", [sys.executable, "-B", "-m", "unittest", "discover", "-s", "tools", "-p", "test_third_party.py"]),
        ("license_screen", [sys.executable, "-B", "tools/third_party.py"]),
        ("build", ["cargo", "build", "--workspace", "--locked"]),
        ("probe_smoke", [sys.executable, "tools/smoke.py", str(binary)]),
        ("reconnect_smoke", [sys.executable, "tools/reconnect_smoke.py", str(binary)]),
        ("context_smoke", [sys.executable, "tools/context_smoke.py", str(binary)]),
    ]
    if args.audit:
        checks.append(("dependency_audit", ["cargo", "audit", "--deny", "warnings", "--json"]))
    def save():
        temporary = output / "report.tmp"
        temporary.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
        temporary.replace(output / "report.json")
    report["checks"] = [{"name": name, "status": "not_run"} for name, _ in checks]
    save()
    if not args.inventory_only:
        report["status"] = "running"
        try:
            for entry, (name, command) in zip(report["checks"], checks):
                print(f"Running {name}", flush=True)
                entry["status"] = "running"
                save()
                with (output / f"{name}.log").open("w", encoding="utf-8") as log:
                    try:
                        result = subprocess.run(command, cwd=ROOT, stdout=log, stderr=subprocess.STDOUT)
                        entry.update(status="passed" if result.returncode == 0 else "failed", exit_code=result.returncode)
                    except OSError as error:
                        log.write(str(error))
                        entry.update(status="failed", exit_code=None)
                save()
                if entry["status"] != "passed":
                    report["status"] = "failed"
                    break
            else:
                report["status"] = "preflight_passed"
        except KeyboardInterrupt:
            report["status"] = "interrupted"
        finally:
            save()
    print(f"{report['status']}: {output / 'report.json'}")
    print("Native desktop/Bluetooth/USB/DPI/permission acceptance: NOT RUN")
    return 0 if report["status"] in ("inventory_only", "preflight_passed") else 1


if __name__ == "__main__":
    raise SystemExit(main())
