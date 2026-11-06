#!/usr/bin/env python3
"""CI-only idle GUI RSS/CPU/optional Darwin footprint diagnostic. No input/network permissions.

Run on a CI desktop (Linux: xvfb-run), not a user's active desktop. RSS is not
Activity Monitor's total memory: compressed memory and some GPU allocations are
not measured by RSS. Darwin optionally adds vmmap physical-footprint values as
separate OS accounting; unavailable values never become zero or a footprint pass.
A bounded sample passing does not establish that a 6 GB incident has been fixed. Only children created by this script are stopped/reaped.
"""
import argparse
from contextlib import contextmanager
from dataclasses import dataclass
import json
import math
import re
from decimal import Decimal, ROUND_CEILING
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


def parse_cpu_time_ns(output):
    """ps TIME is cumulative user+system CPU, with platform-dependent precision."""
    if not isinstance(output, bytes) or len(output) > 64:
        raise ValueError("CPU time unavailable")
    match = re.fullmatch(rb"(?:(\d{1,9})-)?(\d{1,9}):(?:(\d{2}):)?(\d{2})(?:\.(\d{1,9}))?", output.strip())
    if match is None:
        raise ValueError("CPU time unavailable")
    days, first, minutes, seconds, fraction = match.groups()
    first, seconds = int(first), int(seconds)
    if seconds >= 60 or (minutes is not None and int(minutes) >= 60):
        raise ValueError("CPU time unavailable")
    if days is not None and (minutes is None or first >= 24):
        raise ValueError("CPU time unavailable")
    whole = ((int(days or 0) * 24 + first) * 3600 + int(minutes) * 60 + seconds
             if minutes is not None else first * 60 + seconds)
    value = whole * 1_000_000_000 + int((fraction or b"").ljust(9, b"0"))
    if not 0 <= value <= (1 << 63) - 1:
        raise ValueError("CPU time unavailable")
    return value


def sample_cpu_time_ns(pid):
    output = subprocess.check_output(
        ["ps", "-o", "time=", "-p", str(pid)], stderr=subprocess.DEVNULL, timeout=2,
        env={**os.environ, "LC_ALL": "C"}
    )
    return parse_cpu_time_ns(output)



def parse_footprint_summary(output):
    """Extract only two finite numeric lines; discard all paths/summary text."""
    if not isinstance(output, bytes) or len(output) > 256 * 1024:
        raise ValueError("physical footprint unavailable")
    values = {}
    pattern = re.compile(r"^\s*Physical footprint(?P<peak> \(peak\))?:\s*([0-9]+(?:\.[0-9]+)?)\s*([KMGT])(?:B)?\s*$")
    units = {"K": 1, "M": 1024, "G": 1024 ** 2, "T": 1024 ** 3}
    for line in output.decode("utf-8", errors="replace").splitlines():
        match = pattern.fullmatch(line)
        if match is None:
            continue
        key = "reported_peak_kib" if match.group("peak") else "current_kib"
        number, unit = match.group(2), match.group(3)
        if key in values or len(number) > 20:
            raise ValueError("physical footprint unavailable")
        # Round upward to an integral KiB; never round a threshold breach down.
        value = int((Decimal(number) * units[unit]).to_integral_value(rounding=ROUND_CEILING))
        if not 0 <= value <= (1 << 63) - 1:
            raise ValueError("physical footprint unavailable")
        values[key] = value
    if len(values) != 2:
        raise ValueError("physical footprint unavailable")
    return values


def sample_footprint_kib(pid):
    output = subprocess.check_output(
        ["vmmap", "-summary", str(pid)], stderr=subprocess.DEVNULL, timeout=2
    )
    return parse_footprint_summary(output)


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
            clock=time.monotonic, sleep=time.sleep, popen=subprocess.Popen, footprint=None, cpu=None):
    """Injectable supervision; tests use fake RSS or a small Python child, no GUI."""
    stop = stop or StopFlag()
    result = {"scenario": scenario, "pid": None, "passed": False, "reason": "start_failed",
              "elapsed_sec": 0, "peak_rss_kib": 0, "baseline_rss_kib": None,
              "peak_growth_kib": 0, "post_warmup_samples": 0, "samples": [],
              "termination": "not_started", "normal_gui_close": False,
              "exit_status": {"returncode": None, "signal": None}, "stop_signal": 0}
    physical = {"availability": "not_requested" if footprint is None else "unavailable",
                "checks_passed": None, "baseline_kib": None, "baseline_reported_peak_kib": None,
                "peak_kib": None, "reported_peak_kib": None, "peak_growth_kib": None,
                "warmup_baseline_available": False, "final_sample_available": False,
                "post_warmup_samples": 0, "samples": []}
    result["physical_footprint"] = physical
    processor = {"availability": "not_requested" if cpu is None else "unavailable",
                 "sampling_complete": None if cpu is None else False,
                 "peak_interval_percent": None, "average_percent": None,
                 "observed_cpu_sec": None, "covered_wall_sec": None,
                 "intervals": 0, "samples": []}
    result["cpu_usage"] = processor
    previous_cpu = None
    cpu_delta_ns = 0
    cpu_wall_sec = 0
    next_footprint = 0
    warmup_attempted = False
    final_attempted = False
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
            if cpu is not None:
                item = {"elapsed_sec": round(max(0, clock() - started), 3),
                        "availability": "unavailable", "cpu_time_ns": None,
                        "interval_percent": None}
                try:
                    counter = cpu(child.pid)
                    sampled_at = clock()
                    if type(counter) is not int or not 0 <= counter <= (1 << 63) - 1:
                        raise ValueError("CPU time unavailable")
                    if previous_cpu is not None:
                        wall = sampled_at - previous_cpu[0]
                        delta = counter - previous_cpu[1]
                        if not math.isfinite(wall) or wall <= 0 or delta < 0:
                            raise ValueError("CPU interval unavailable")
                        percent = 100 * delta / 1_000_000_000 / wall
                        if not math.isfinite(percent):
                            raise ValueError("CPU interval unavailable")
                        item["interval_percent"] = round(percent, 6)
                        processor["peak_interval_percent"] = (percent if processor["peak_interval_percent"] is None
                                                              else max(processor["peak_interval_percent"], percent))
                        cpu_delta_ns += delta
                        cpu_wall_sec += wall
                        processor["intervals"] += 1
                    previous_cpu = (sampled_at, counter)
                    item.update(elapsed_sec=round(max(0, sampled_at - started), 3),
                                availability="available", cpu_time_ns=counter)
                except Exception:
                    previous_cpu = None  # Never interpolate across missing/regressed counters.
                processor["samples"].append(item)
                elapsed = max(0, clock() - started)
                if stop.signum:
                    result["reason"] = "interrupted"
                    break
            is_warmup = elapsed >= limits.warmup and not warmup_attempted
            is_final = elapsed >= limits.duration
            if footprint is not None and (elapsed >= next_footprint or is_warmup or is_final):
                warmup_attempted = warmup_attempted or is_warmup
                final_attempted = final_attempted or is_final
                next_footprint = elapsed + 15
                item = {"elapsed_sec": round(elapsed, 3), "availability": "unavailable"}
                try:
                    measured = footprint(child.pid)
                    current, peak = measured["current_kib"], measured["reported_peak_kib"]
                    if not all(isinstance(value, int) and 0 <= value <= (1 << 63) - 1
                               for value in (current, peak)):
                        raise ValueError("physical footprint unavailable")
                    item.update(availability="available", current_kib=current, reported_peak_kib=peak)
                    physical["peak_kib"] = current if physical["peak_kib"] is None else max(physical["peak_kib"], current)
                    physical["reported_peak_kib"] = peak if physical["reported_peak_kib"] is None else max(physical["reported_peak_kib"], peak)
                    if is_warmup:
                        physical["warmup_baseline_available"] = True
                    if is_final:
                        physical["final_sample_available"] = True
                    if elapsed >= limits.warmup:
                        physical["post_warmup_samples"] += 1
                        if physical["baseline_kib"] is None:
                            physical["baseline_kib"] = current
                            physical["baseline_reported_peak_kib"] = peak
                        growth = max(0, current - physical["baseline_kib"],
                                     peak - physical["baseline_reported_peak_kib"])
                        physical["peak_growth_kib"] = growth if physical["peak_growth_kib"] is None else max(physical["peak_growth_kib"], growth)
                    if max(current, peak) > limits.rss_limit_mib * 1024:
                        result["reason"] = "physical_footprint_limit"
                    elif physical["peak_growth_kib"] is not None and physical["peak_growth_kib"] > limits.growth_limit_mib * 1024:
                        result["reason"] = "physical_footprint_growth_limit"
                except Exception:
                    pass  # unavailable, never persist summary/paths/error text or invented zero
                physical["samples"].append(item)
                if result["reason"] in ("physical_footprint_limit", "physical_footprint_growth_limit"):
                    break
                elapsed = max(0, clock() - started)
            if stop.signum:
                result["reason"] = "interrupted"
                break
            if elapsed >= limits.duration and (footprint is None or final_attempted):
                result["reason"] = "duration_reached"
                break
            sleep(min(limits.interval, max(0, limits.duration - elapsed)))
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
        result["rss_checks_passed"] = result["passed"]
        if result["reason"] == "duration_reached" and result["post_warmup_samples"] < 2:
            result["reason"] = "insufficient_post_warmup_samples"
        if footprint is not None:
            successful = sum(item["availability"] == "available" for item in physical["samples"])
            complete = (physical["warmup_baseline_available"] and physical["final_sample_available"]
                        and physical["post_warmup_samples"] >= 2
                        and successful == len(physical["samples"]))
            physical["availability"] = "available" if complete else "partial" if successful else "unavailable"
            if result["reason"] in ("physical_footprint_limit", "physical_footprint_growth_limit"):
                physical["checks_passed"] = False
            elif result["passed"] and complete:
                physical["checks_passed"] = True
        if cpu is not None:
            available = sum(item["availability"] == "available" for item in processor["samples"])
            complete = (result["rss_checks_passed"] and available == len(processor["samples"])
                        and available == len(result["samples"]) and processor["intervals"] >= 2)
            processor["sampling_complete"] = complete
            processor["availability"] = "available" if complete else "partial" if available else "unavailable"
            if processor["intervals"]:
                processor["observed_cpu_sec"] = round(cpu_delta_ns / 1_000_000_000, 9)
                processor["covered_wall_sec"] = round(cpu_wall_sec, 6)
                processor["average_percent"] = round(100 * cpu_delta_ns / 1_000_000_000 / cpu_wall_sec, 6)
            if result["passed"] and not complete:
                result["passed"] = False
                result["reason"] = "cpu_sample_incomplete"
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
                if initialization["reason"] == "identity_created":
                    materials = [profile / "identity" / name for name in ("node.crt.der", "node.key.der")]
                    if not all(path.is_file() and path.stat().st_size > 0 for path in materials):
                        initialization["reason"] = "identity_files_missing"
                if initialization["reason"] != "identity_created":
                    results.append({"scenario": scenario, "pid": None, "passed": False,
                                    "reason": "identity_init_failed", "initialization": initialization})
                    continue
            result = monitor([str(binary_dir / "peerglide-gui"), "--data-dir", str(profile)],
                             limits, scenario=scenario, env=env, stop=stop,
                             cpu=sample_cpu_time_ns,
                             footprint=sample_footprint_kib if sys.platform == "darwin" else None)
            if initialization is not None:
                result["initialization"] = initialization
            results.append(result)
    return results


def report(limits, scenarios, reason):
    physical_results = [item.get("physical_footprint", {}).get("checks_passed") for item in scenarios]
    physical_passed = (False if False in physical_results else
                       True if len(physical_results) == 2 and all(value is True for value in physical_results) else None)
    coverage = "rss_and_physical_footprint" if physical_passed is True else "rss_only_or_partial_physical_footprint"
    cpu_results = [item.get("cpu_usage", {}).get("sampling_complete") for item in scenarios]
    cpu_complete = (False if False in cpu_results else
                    True if len(cpu_results) == 2 and all(value is True for value in cpu_results) else None)
    if cpu_complete is True:
        coverage = "rss_cpu_and_physical_footprint" if physical_passed is True else "rss_cpu_optional_physical_footprint"
    return {"schema_version": 2, "scope": "isolated_ci_gui_resources", "measurement": "ps_rss_kib", "optional_measurement": "vmmap_physical_footprint_kib",
            "cpu_measurement": "ps_cumulative_cpu_time", "cpu_sampling_complete": cpu_complete,
            "coverage": coverage, "physical_footprint_checks_passed": physical_passed,
            "limitations": ["rss_excludes_compressed_memory", "some_gpu_memory_not_measured", "sampled_peak_not_process_lifetime_peak",
                            "resident_process_only_not_render_or_native_acceptance", "rss_and_footprint_are_distinct_accounting_scopes",
                            "cpu_percent_100_equals_one_logical_cpu", "ps_cpu_time_platform_precision",
                            "cpu_does_not_measure_gpu_load", "owned_gui_process_only_not_worker_tree"],
            "passed": reason == "completed" and len(scenarios) == 2 and cpu_complete is True
                      and all(item["passed"] for item in scenarios),
            "rss_checks_passed": reason == "completed" and len(scenarios) == 2
                                 and all(item.get("rss_checks_passed", False) for item in scenarios),
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
    if value["passed"]:
        print("PASS: RSS limits and CPU sampling complete; physical footprint sampled within limits" if value["physical_footprint_checks_passed"] is True
              else "PASS: RSS limits and CPU sampling complete; physical footprint unavailable or incomplete")
    else:
        print("FAIL: GUI memory diagnostic")
    return 0 if value["passed"] else 1


if __name__ == "__main__":
    sys.exit(main())
