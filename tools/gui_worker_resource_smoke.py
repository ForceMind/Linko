#!/usr/bin/env python3
"""CI-only actual GUI/owned communication worker accounting. No native input."""
import argparse
import json
import math
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import time

import gui_memory_smoke as metrics


def records(path, final=False):
    if not path.exists():
        return []
    raw = path.read_bytes()
    if len(raw) > 8192 or (final and raw and not raw.endswith(b"\n")):
        raise ValueError("resource receipt invalid")
    complete = raw[:raw.rfind(b"\n") + 1] if not raw.endswith(b"\n") else raw
    result = []
    for line in complete.splitlines():
        item = json.loads(line)
        if (set(item) != {"schema_version", "scope", "state", "worker_pid", "worker_reaped", "elapsed_sec", "native",
                          "window_state", "tray_show_received", "tray_stop_received"}
                or item["schema_version"] != 2 or item["scope"] != "ci_owned_gui_communication_resources"
                or item["native"] is not False or not math.isfinite(item["elapsed_sec"])
                or item["elapsed_sec"] < 0 or item["state"] not in {"prepared", "visible", "connected", "shown", "stopping", "completed", "failed"}
                or item["window_state"] not in {"normal_window_requested", "appkit_visible", "appkit_hidden"}
                or type(item["tray_show_received"]) is not bool or type(item["tray_stop_received"]) is not bool):
            raise ValueError("resource receipt invalid")
        result.append(item)
    return result


def hidden_visibility_proof(receipts):
    states = [r["state"] for r in receipts]
    if states != ["prepared", "visible", "connected", "shown", "stopping", "completed"]:
        raise ValueError("hidden lifecycle incomplete")
    before, sampled, after, completed = receipts[1], receipts[2], receipts[3], receipts[-1]
    if (before["window_state"] != "appkit_visible" or sampled["window_state"] != "appkit_hidden"
            or after["window_state"] != "appkit_visible" or not after["tray_show_received"]
            or completed["window_state"] != "appkit_visible" or not completed["tray_show_received"]
            or not completed["tray_stop_received"] or not completed["worker_reaped"]):
        raise ValueError("hidden lifecycle unverified")
    return {"before": "appkit_visible", "sampling": "appkit_hidden", "after": "appkit_visible",
            "tray_show_received": True, "tray_stop_received": True}


def parent_pid(pid):
    value = subprocess.check_output(["ps", "-o", "ppid=", "-p", str(pid)], stderr=subprocess.DEVNULL, timeout=2).strip()
    if not value.isdigit() or len(value) > 20:
        raise ValueError("worker parent unavailable")
    return int(value)


def wait_for(test, child, timeout, stop):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if stop.signum:
            raise ValueError("resource check interrupted")
        result = test()
        if result:
            return result
        if child.poll() is not None:
            raise ValueError("owned process exited early")
        time.sleep(0.05)
    raise TimeoutError("resource check timeout")


def run(binary_dir, seconds, stop, tray_hidden=False, screenshot=None):
    result = {"schema_version": 2, "scope": "ci_gui_owned_worker_resources", "passed": False,
              "reason": "start_failed", "native": False, "visibility": "hidden_unverified" if tray_hidden else "normal_window_requested",
              "window_visibility_proof": None,
              "lifecycle": None,
              "processes": {}, "gui_exit": None, "helper_exit": None, "worker_reaped": False,
              "limitations": ["no_native_input", "not_physical_acceptance", "visibility_not_os_verified",
                              "ps_cpu_time_platform_precision", "rss_is_not_total_memory", "physical_footprint_not_sampled",
                              "not_eight_hour_endurance"]}
    gui = helper = receipt = None
    stage = "identity_init"
    with tempfile.TemporaryDirectory(prefix="peerglide-owned-resources-") as temporary:
        root = Path(temporary)
        profile, remote = root / "profile", root / "remote"
        profile.mkdir()
        env = {**os.environ, "XDG_DATA_HOME": str(root / "fallback"), "XDG_CACHE_HOME": str(root / "cache")}
        worker = binary_dir / ("peerglide.exe" if sys.platform == "win32" else "peerglide")
        try:
            for identity in [profile / "identity", remote]:
                subprocess.run([str(worker), "init", "--identity", str(identity)], check=True,
                               stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=10)
            log = root / "helper.log"
            with log.open("w") as output:
                helper = subprocess.Popen([str(worker), "listen", "--identity", str(remote),
                    "--peer-cert", str(profile / "identity/node.crt.der"), "--bind", "127.0.0.1:0", "--managed"],
                    stdin=subprocess.PIPE, stdout=output, stderr=subprocess.STDOUT, env=env)
            stage = "helper_start"
            address = wait_for(lambda: re.search(r"Listening on (127\.0\.0\.1:\d+)", log.read_text()), helper, 10, stop)[1]
            receipt, spec = root / "receipt.jsonl", root / "spec.json"
            spec.write_text(json.dumps({"schema_version": 1, "address": address,
                "peer_certificate": str(remote / "node.crt.der"), "report": str(receipt), "seconds": seconds,
                "tray_hidden": tray_hidden}))
            gui = subprocess.Popen([str(binary_dir / ("peerglide-gui.exe" if sys.platform == "win32" else "peerglide-gui")),
                "--data-dir", str(profile), "--ci-resource-spec", str(spec)], stdin=subprocess.PIPE,
                stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, env=env)
            stage = "gui_connect"
            connected = wait_for(lambda: next((r for r in records(receipt) if r["state"] == "connected"), None), gui, 25 if tray_hidden else 20, stop)
            if tray_hidden and connected["window_state"] != "appkit_hidden":
                raise ValueError("hidden window not verified")
            owned_worker = connected["worker_pid"]
            if type(owned_worker) is not int or owned_worker <= 0 or parent_pid(owned_worker) != gui.pid:
                raise ValueError("owned worker not established")
            if screenshot is not None:
                stage = "screenshot"
                if sys.platform != "linux" or tray_hidden or not os.environ.get("DISPLAY"):
                    raise ValueError("isolated screenshot unavailable")
                # CI invokes us under its own fresh Xvfb. This has only the
                # new fixture GUI, never a user's desktop or real device data.
                time.sleep(1)
                fd = os.open(screenshot, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
                with os.fdopen(fd, "wb") as output:
                    subprocess.run(["import", "-window", "root", "png:-"], stdout=output,
                                   stderr=subprocess.DEVNULL, check=True, timeout=10)
                if not 1000 < screenshot.stat().st_size <= 16 * 1024 * 1024:
                    raise ValueError("isolated screenshot invalid")
            roles = {"gui": gui.pid, "owned_worker": owned_worker, "peer_helper": helper.pid}
            previous = {}
            stage = "sampling"
            started = time.monotonic()
            for role, pid in roles.items():
                result["processes"][role] = {"pid": pid, "peak_rss_kib": 0, "samples": [],
                                              "average_cpu_percent": None, "covered_wall_sec": None}
            while True:
                if stop.signum:
                    raise ValueError("resource check interrupted")
                if gui.poll() is not None or helper.poll() is not None or parent_pid(owned_worker) != gui.pid:
                    raise ValueError("resource process ended during sampling")
                if any(r["state"] == "failed" for r in records(receipt)):
                    raise ValueError("GUI resource check failed")
                for role, pid in roles.items():
                    item = result["processes"][role]
                    rss = metrics.sample_rss_kib(pid)
                    if rss > 512 * 1024:
                        raise ValueError("resource RSS guard")
                    cpu = metrics.sample_cpu_time_ns(pid)
                    at = time.monotonic()
                    percent = None
                    if role in previous:
                        wall = at - previous[role][0]
                        delta = cpu - previous[role][1]
                        if wall <= 0 or delta < 0:
                            raise ValueError("CPU accounting unavailable")
                        percent = 100 * delta / 1_000_000_000 / wall
                        if not math.isfinite(percent):
                            raise ValueError("CPU accounting unavailable")
                    previous[role] = (at, cpu)
                    item["peak_rss_kib"] = max(item["peak_rss_kib"], rss)
                    item["samples"].append({"elapsed_sec": at - started, "rss_kib": rss,
                                             "cpu_time_ns": cpu, "interval_percent": percent})
                elapsed = time.monotonic() - started
                if elapsed >= seconds:
                    break
                time.sleep(min(1, seconds - elapsed))
            for item in result["processes"].values():
                first, last = item["samples"][0], item["samples"][-1]
                wall = last["elapsed_sec"] - first["elapsed_sec"]
                if len(item["samples"]) < 3 or wall <= 0:
                    raise ValueError("insufficient accounting")
                item["covered_wall_sec"] = wall
                item["average_cpu_percent"] = 100 * (last["cpu_time_ns"] - first["cpu_time_ns"]) / 1_000_000_000 / wall
            stage = "gui_stop"
            gui.stdin.write(b"stop\n"); gui.stdin.flush()
            result["gui_exit"] = gui.wait(timeout=10)
            final = records(receipt, final=True)
            result["worker_reaped"] = bool(final and final[-1]["state"] == "completed" and final[-1]["worker_reaped"])
            if result["gui_exit"] != 0 or not result["worker_reaped"]:
                raise ValueError("GUI did not cooperatively reap worker")
            if tray_hidden:
                result["window_visibility_proof"] = hidden_visibility_proof(final)
                result["visibility"] = "appkit_hidden_verified"
                result["limitations"].remove("visibility_not_os_verified")
                result["limitations"].extend(["appkit_visibility_not_unobscured_pixels", "not_physical_menu_click"])
            stage = "helper_stop"
            if helper.poll() is None:
                helper.stdin.write(b"stop\n"); helper.stdin.flush()
            result["helper_exit"] = helper.wait(timeout=5)
            if result["helper_exit"] != 0:
                raise ValueError("helper cleanup failed")
            result.update(passed=True, reason="completed")
        except Exception:
            result["reason"] = "interrupted" if stop.signum else stage + "_failed"
        finally:
            for child in [gui, helper]:
                if child is not None:
                    if child.poll() is None:
                        try:
                            child.stdin.write(b"stop\n"); child.stdin.flush(); child.wait(timeout=5)
                        except (OSError, subprocess.TimeoutExpired):
                            pass
                    metrics.stop_child(child, 2)  # Only our NoNative Popen handles.
            if receipt is not None:
                try:
                    # Keep only the bounded, allowlisted stage metadata when
                    # a new native transition fails. Never preserve GUI logs.
                    result["lifecycle"] = records(receipt, final=True)
                except (ValueError, OSError, TypeError):
                    pass
    return result


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary_dir", type=Path)
    parser.add_argument("--report", type=Path, required=True)
    parser.add_argument("--duration", type=int, default=30)
    parser.add_argument("--tray-hidden", action="store_true", help="macOS only: verify real close/hide/menu show/stop")
    parser.add_argument("--screenshot", type=Path, help="CI-owned Linux Xvfb only, exclusive new PNG")
    args = parser.parse_args(argv)
    if not 17 <= args.duration <= 180:
        parser.error("duration must be between 17 and 180 seconds")
    try:
        fd = os.open(args.report, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    except OSError:
        return 1
    with os.fdopen(fd, "w") as output:
        if os.environ.get("CI", "").lower() not in ("true", "1") or sys.platform not in ("darwin", "linux"):
            value = {"schema_version": 2, "passed": False, "reason": "isolated_ci_required"}
        elif args.tray_hidden and sys.platform != "darwin":
            value = {"schema_version": 2, "passed": False, "reason": "hidden_check_macos_required"}
        else:
            stop = metrics.StopFlag()
            with metrics.signal_handlers(stop):
                value = run(args.binary_dir.resolve(), args.duration, stop, args.tray_hidden, args.screenshot)
        metrics.write_report(output, value)
    print("PASS: actual GUI and owned NoNative worker accounted and reaped" if value["passed"] else "FAIL: owned resource accounting")
    return 0 if value["passed"] else 1


if __name__ == "__main__":
    sys.exit(main())
