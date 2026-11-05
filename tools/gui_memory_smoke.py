#!/usr/bin/env python3
"""CI-only idle GUI RSS diagnostic. Never enables input, networking or permissions.

Run on a CI desktop (Linux: xvfb-run), not a user's active desktop. RSS is not
Activity Monitor's total memory: compressed memory and some GPU allocations are
not measured. A bounded sample passing does not establish that a 6 GB incident
has been fixed. Only children created by this script are stopped/reaped.
"""
import argparse
from contextlib import contextmanager
from dataclasses import dataclass
import json
import math
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time

MAX_SAMPLES = 3602
MAX_REPORT_BYTES = 2 * 1024 * 1024


@dataclass(frozen=True)
class Limits:
    duration: float = 120
    rss_limit_mib: float = 512
    growth_limit_mib: float = 128
    warmup: float = 15
    interval: float = 1
    stop_timeout: float = 2
    init_timeout: float = 10

    def __post_init__(self):
        for value in (self.duration, self.rss_limit_mib, self.growth_limit_mib,
                      self.warmup, self.interval, self.stop_timeout, self.init_timeout):
            if not math.isfinite(value) or value < 0:
                raise ValueError("invalid limits")
        if not (0 < self.duration <= 3600 and self.rss_limit_mib > 0
                and self.interval > 0 and self.stop_timeout > 0 and self.init_timeout > 0):
            raise ValueError("invalid limits")


@dataclass
class StopFlag:
    signum: int = 0


@contextmanager
def signal_handlers(stop):
    previous = {}

    def request_stop(signum, _frame):
        stop.signum = signum  # no exceptions during Popen/child registration

    try:
        for name in ("SIGINT", "SIGTERM"):
            number = getattr(signal, name, None)
            if number is not None:
                previous[number] = signal.signal(number, request_stop)
        yield
    finally:
        for number, handler in previous.items():
            signal.signal(number, handler)


def sample_rss_kib(pid):
    """ps RSS uses KiB on Darwin/Linux. This is exactly our own Popen child PID."""
    output = subprocess.check_output(
        ["ps", "-o", "rss=", "-p", str(pid)], stderr=subprocess.DEVNULL, timeout=2
    )
    value = output.strip()
    if len(value) > 20 or not value.isdigit():
        raise ValueError("RSS unavailable")
    rss = int(value)
    if not 0 <= rss <= (1 << 63) - 1:
        raise ValueError("RSS unavailable")
    return rss


def stop_child(child, timeout):
    """No process groups/killall: terminate, bounded wait, kill this child only."""
    if child.poll() is not None:
        child.wait(timeout=timeout)
        return "already_exited"
    try:
        child.terminate()
        child.wait(timeout=timeout)
        return "terminated"
    except (subprocess.TimeoutExpired, OSError):
        try:
            if child.poll() is None:
                child.kill()
            child.wait(timeout=timeout)
            return "killed"
        except (subprocess.TimeoutExpired, OSError):
            return "cleanup_failed"


def exit_status(child):
    code = None if child is None else child.poll()
    return {"returncode": code, "signal": -code if code is not None and code < 0 else None}


def monitor(command, limits, *, scenario, env=None, stop=None, sample=sample_rss_kib,
            clock=time.monotonic, sleep=time.sleep, popen=subprocess.Popen):
    """Injectable supervision; tests use fake RSS or a small Python child, no GUI."""
    stop = stop or StopFlag()
    result = {"scenario": scenario, "pid": None, "passed": False, "reason": "start_failed",
              "elapsed_sec": 0, "peak_rss_kib": 0, "baseline_rss_kib": None,
              "peak_growth_kib": 0, "post_warmup_samples": 0, "samples": [],
              "termination": "not_started", "normal_gui_close": False,
              "exit_status": {"returncode": None, "signal": None}, "stop_signal": 0}
    child = None
    started = clock()
    try:
        if stop.signum:
            result["reason"] = "interrupted"
            return result
        try:
            child = popen(command, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                          stderr=subprocess.DEVNULL, env=env)
        except OSError:
            return result  # never include binary/profile/error paths
        result["pid"] = child.pid
        while True:
            if stop.signum:
                result["reason"] = "interrupted"
                break
            if child.poll() is not None:
                result["reason"] = "early_exit"
                break
            if len(result["samples"]) >= MAX_SAMPLES:
                result["reason"] = "sample_limit"
                break
            try:
                rss = sample(child.pid)
                if not isinstance(rss, int) or rss < 0 or rss > (1 << 63) - 1:
                    raise ValueError("RSS unavailable")
            except Exception:
                result["reason"] = "early_exit" if child.poll() is not None else "sample_failed"
                break
            elapsed = max(0, clock() - started)
            result["samples"].append({"elapsed_sec": round(elapsed, 3), "rss_kib": rss})
            result["peak_rss_kib"] = max(result["peak_rss_kib"], rss)
            # Absolute guard includes startup/warmup and precedes any sleeping.
            if rss > limits.rss_limit_mib * 1024:
                result["reason"] = "rss_limit"
                break
            if elapsed >= limits.warmup:
                result["post_warmup_samples"] += 1
                if result["baseline_rss_kib"] is None:
                    result["baseline_rss_kib"] = rss
                growth = max(0, rss - result["baseline_rss_kib"])
                result["peak_growth_kib"] = max(result["peak_growth_kib"], growth)
                if growth > limits.growth_limit_mib * 1024:
                    result["reason"] = "growth_limit"
                    break
            if stop.signum:
                result["reason"] = "interrupted"
                break
            if elapsed >= limits.duration:
                result["reason"] = "duration_reached"
                break
            sleep(min(limits.interval, limits.duration - elapsed))
    except KeyboardInterrupt:
        stop.signum = signal.SIGINT
        result["reason"] = "interrupted"
    finally:
        result["elapsed_sec"] = round(max(0, clock() - started), 3)
        result["stop_signal"] = stop.signum
        if child is not None:
            result["termination"] = stop_child(child, limits.stop_timeout)
            result["exit_status"] = exit_status(child)
        result["passed"] = (result["reason"] == "duration_reached"
                            and result["post_warmup_samples"] >= 2
                            and result["termination"] == "terminated")
        if result["reason"] == "duration_reached" and result["post_warmup_samples"] < 2:
            result["reason"] = "insufficient_post_warmup_samples"
    return result


def initialize_identity(command, limits, stop, env):
    child = None
    reason = "identity_start_failed"
    termination = "not_started"
    started = time.monotonic()
    try:
        if stop.signum:
            return {"reason": "interrupted", "pid": None, "termination": termination,
                    "exit_status": exit_status(None)}
        try:
            child = subprocess.Popen(command, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                                     stderr=subprocess.DEVNULL, env=env)
        except OSError:
            pass
        if child is not None:
            while child.poll() is None:
                if stop.signum:
                    reason = "interrupted"
                    break
                if time.monotonic() - started >= limits.init_timeout:
                    reason = "identity_timeout"
                    break
                time.sleep(0.05)
            else:
                reason = "identity_created" if child.returncode == 0 else "identity_failed"
    finally:
        if child is not None:
            termination = stop_child(child, limits.stop_timeout)
    return {"reason": reason, "pid": None if child is None else child.pid,
            "termination": termination, "exit_status": exit_status(child)}


def run_scenarios(binary_dir, limits, stop):
    results = []
    for scenario in ("empty_identity", "existing_identity"):
        if stop.signum:
            break
        with tempfile.TemporaryDirectory(prefix="peerglide-memory-ci-") as temporary:
            base = Path(temporary)
            profile = base / "profile"
            profile.mkdir()
            # Explicit profile and temporary fallback/cache roots. No existing
            # user profile, trust, identity, native flag or network command.
            env = os.environ.copy()
            env.update(XDG_DATA_HOME=str(base / "fallback"), XDG_CACHE_HOME=str(base / "cache"))
            initialization = None
            if scenario == "existing_identity":
                initialization = initialize_identity(
                    [str(binary_dir / "peerglide"), "init", "--identity", str(profile / "identity")],
                    limits, stop, env)
                if initialization["reason"] != "identity_created":
                    results.append({"scenario": scenario, "pid": None, "passed": False,
                                    "reason": "identity_init_failed", "initialization": initialization})
                    continue
            result = monitor([str(binary_dir / "peerglide-gui"), "--data-dir", str(profile)],
                             limits, scenario=scenario, env=env, stop=stop)
            if initialization is not None:
                result["initialization"] = initialization
            results.append(result)
    return results


def report(limits, scenarios, reason):
    return {"schema_version": 1, "scope": "isolated_ci_gui_rss", "measurement": "ps_rss_kib",
            "limitations": ["compressed_memory_not_measured", "some_gpu_memory_not_measured",
                            "resident_process_only_not_render_or_native_acceptance"],
            "passed": reason == "completed" and len(scenarios) == 2
                      and all(item["passed"] for item in scenarios),
            "reason": reason, "limits": vars(limits), "scenarios": scenarios}


def write_report(output, value):
    data = json.dumps(value, ensure_ascii=True, separators=(",", ":"), allow_nan=False)
    if len(data.encode("utf-8")) > MAX_REPORT_BYTES:
        raise ValueError("report limit")
    output.write(data + "\n")
    output.flush()


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary_dir", type=Path)
    parser.add_argument("--report", type=Path, required=True, help="new report file; never overwritten")
    parser.add_argument("--duration", type=float, default=120)
    parser.add_argument("--rss-limit-mib", type=float, default=512)
    parser.add_argument("--growth-limit-mib", type=float, default=128)
    args = parser.parse_args(argv)
    if not math.isfinite(args.duration) or not 17 <= args.duration <= 3600:
        parser.error("duration must be between 17 and 3600 seconds")
    if (not math.isfinite(args.rss_limit_mib) or not 0 < args.rss_limit_mib <= 16384
            or not math.isfinite(args.growth_limit_mib) or not 0 <= args.growth_limit_mib <= 16384):
        parser.error("memory limits must be finite and within 0..16384 MiB (RSS must be positive)")
    limits = Limits(args.duration, args.rss_limit_mib, args.growth_limit_mib)
    try:
        # Create before starting children. An existing file/symlink is rejected.
        descriptor = os.open(args.report, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    except OSError:
        print("FAIL: report_create_failed", file=sys.stderr)
        return 1
    with os.fdopen(descriptor, "w", encoding="utf-8") as output:
        scenarios = []
        stop = StopFlag()
        reason = "completed"
        try:
            if os.environ.get("CI", "").lower() not in ("true", "1"):
                reason = "ci_environment_required"
            elif sys.platform not in ("darwin", "linux"):
                reason = "unsupported_host"
            else:
                with signal_handlers(stop):
                    scenarios = run_scenarios(args.binary_dir.resolve(), limits, stop)
                if stop.signum:
                    reason = "interrupted"
        except Exception:
            reason = "harness_failed"  # no external paths, logs or exception text
        value = report(limits, scenarios, reason)
        write_report(output, value)
    print("PASS: isolated GUI RSS samples within limits" if value["passed"] else "FAIL: GUI RSS diagnostic")
    return 0 if value["passed"] else 1


if __name__ == "__main__":
    sys.exit(main())
