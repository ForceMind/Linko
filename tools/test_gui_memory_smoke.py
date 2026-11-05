"""Bounded GUI-memory supervision tests; never starts a GUI/native input."""
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

import gui_memory_smoke as smoke


class Clock:
    def __init__(self):
        self.now = 0

    def __call__(self):
        return self.now

    def sleep(self, seconds):
        self.now += seconds


class FakeChild:
    pid = 12345  # Pure handle fixture, never used with an OS kill/query.

    def __init__(self, code=None, ignore_term=False):
        self.returncode = code
        self.ignore_term = ignore_term
        self.terminates = 0
        self.kills = 0
        self.waits = 0

    def poll(self):
        return self.returncode

    def terminate(self):
        self.terminates += 1
        if not self.ignore_term:
            self.returncode = -15

    def kill(self):
        self.kills += 1
        self.returncode = -9

    def wait(self, timeout):
        self.waits += 1
        if self.returncode is None:
            raise subprocess.TimeoutExpired("fake child", timeout)
        return self.returncode


class MemorySmoke(unittest.TestCase):
    def monitor_fake(self, values, *, limits=None, child=None, stop=None, footprint=None):
        clock = Clock()
        child = child or FakeChild()
        samples = iter(values)
        result = smoke.monitor(["fixture"], limits or smoke.Limits(duration=4, warmup=1),
                               scenario="fixture", stop=stop, sample=lambda _pid: next(samples),
                               clock=clock, sleep=clock.sleep, popen=lambda *_a, **_k: child, footprint=footprint)
        return result, child, clock

    def test_absolute_cap_applies_during_warmup_and_stops_without_sleeping(self):
        result, child, clock = self.monitor_fake([513 * 1024],
            limits=smoke.Limits(duration=120, warmup=15, rss_limit_mib=512))
        self.assertEqual(result["reason"], "rss_limit")
        self.assertEqual(clock.now, 0)
        self.assertEqual(result["post_warmup_samples"], 0)
        self.assertEqual(child.terminates, 1)
        self.assertEqual(child.kills, 0)
        self.assertFalse(result["passed"])

    def test_growth_uses_first_post_warmup_sample_and_inclusive_limits(self):
        limits = smoke.Limits(duration=4, warmup=1, rss_limit_mib=100, growth_limit_mib=5)
        result, child, _ = self.monitor_fake([90 * 1024, 10 * 1024, 15 * 1024, 16 * 1024], limits=limits)
        self.assertEqual(result["reason"], "growth_limit")
        self.assertEqual(result["baseline_rss_kib"], 10 * 1024)
        self.assertEqual(result["peak_growth_kib"], 6 * 1024)
        self.assertEqual(result["peak_rss_kib"], 90 * 1024)
        self.assertEqual(child.terminates, 1)

    def test_reaching_duration_is_measurement_completion_not_a_normal_gui_close(self):
        result, child, _ = self.monitor_fake([512 * 1024] * 5)
        self.assertTrue(result["passed"])
        self.assertEqual(result["reason"], "duration_reached")
        self.assertEqual(result["termination"], "terminated")
        self.assertFalse(result["normal_gui_close"])
        self.assertEqual(child.waits, 1)

    def test_insufficient_warmup_or_sampler_failure_never_passes_and_reaps(self):
        result, child, _ = self.monitor_fake([1024] * 5,
            limits=smoke.Limits(duration=4, warmup=15))
        self.assertEqual(result["reason"], "insufficient_post_warmup_samples")
        self.assertFalse(result["passed"])
        self.assertIsNotNone(child.poll())
        result, child, _ = self.monitor_fake([])
        self.assertEqual(result["reason"], "sample_failed")
        self.assertIsNotNone(child.poll())

    def test_failed_start_and_early_exit_do_not_become_memory_passes_or_echo_paths(self):
        def fail(*_args, **_kwargs):
            raise FileNotFoundError("private/user/path")
        result = smoke.monitor(["private/user/path"], smoke.Limits(), scenario="fixture", popen=fail)
        self.assertEqual(result["reason"], "start_failed")
        self.assertIsNone(result["pid"])
        self.assertNotIn("private/user/path", json.dumps(result))
        for code in (0, 143, -15):
            result, child, _ = self.monitor_fake([], child=FakeChild(code=code))
            self.assertEqual(result["reason"], "early_exit")
            self.assertEqual(result["exit_status"]["returncode"], code)
            self.assertFalse(result["passed"])
            self.assertEqual(child.terminates, 0)
            self.assertEqual(child.waits, 1)

    def test_termination_timeout_kills_only_the_owned_handle_and_fails(self):
        result, child, _ = self.monitor_fake([1024] * 5, child=FakeChild(ignore_term=True))
        self.assertEqual(child.terminates, 1)
        self.assertEqual(child.kills, 1)
        self.assertEqual(child.waits, 2)
        self.assertEqual(result["termination"], "killed")
        self.assertFalse(result["normal_gui_close"])
        self.assertFalse(result["passed"])

    def test_small_real_python_child_is_terminated_and_waited_without_gui_or_ps(self):
        children = []
        def start(*args, **kwargs):
            child = subprocess.Popen(*args, **kwargs)
            children.append(child)
            return child
        result = smoke.monitor([sys.executable, "-c", "import time; time.sleep(60)"],
            smoke.Limits(duration=0.06, warmup=0, interval=0.01, stop_timeout=1),
            scenario="python_fixture", sample=lambda _pid: 1024, popen=start)
        self.assertEqual(result["reason"], "duration_reached")
        self.assertTrue(result["passed"])
        self.assertEqual(len(children), 1)
        self.assertIsNotNone(children[0].poll())
        self.assertEqual(children[0].wait(timeout=1), result["exit_status"]["returncode"])

    @unittest.skipUnless(os.name == "posix", "POSIX signal-handler integration")
    def test_real_signal_requests_cleanup_and_handlers_are_restored(self):
        stop = smoke.StopFlag()
        previous = signal.getsignal(signal.SIGTERM)
        children = []
        def start(*args, **kwargs):
            child = subprocess.Popen(*args, **kwargs)
            children.append(child)
            return child
        def sample(_pid):
            signal.raise_signal(signal.SIGTERM)
            return 1024
        with smoke.signal_handlers(stop):
            result = smoke.monitor([sys.executable, "-c", "import time; time.sleep(60)"],
                smoke.Limits(duration=30, stop_timeout=1), scenario="python_fixture",
                stop=stop, sample=sample, popen=start)
        self.assertEqual(signal.getsignal(signal.SIGTERM), previous)
        self.assertEqual(result["reason"], "interrupted")
        self.assertEqual(result["stop_signal"], signal.SIGTERM)
        self.assertIsNotNone(children[0].poll())
        self.assertFalse(result["passed"])

    def test_ps_parser_accepts_only_one_finite_rss_value(self):
        with patch.object(smoke.subprocess, "check_output", return_value=b" 12345\n") as query:
            self.assertEqual(smoke.sample_rss_kib(555), 12345)
            self.assertEqual(query.call_args.args[0], ["ps", "-o", "rss=", "-p", "555"])
            self.assertEqual(query.call_args.kwargs["timeout"], 2)
        for invalid in (b"", b"1\n2", b"private/user/path", b"-1", b"9" * 30):
            with patch.object(smoke.subprocess, "check_output", return_value=invalid):
                with self.assertRaises(ValueError):
                    smoke.sample_rss_kib(555)

    def test_new_report_is_exclusive_and_non_ci_launch_is_refused(self):
        with tempfile.TemporaryDirectory() as folder:
            output = Path(folder) / "new.json"
            with patch.dict(os.environ, {"CI": "false"}), patch.object(smoke, "run_scenarios") as run, \
                    patch("builtins.print"):
                self.assertEqual(smoke.main([folder, "--report", str(output)]), 1)
                run.assert_not_called()
            value = json.loads(output.read_text())
            self.assertFalse(value["passed"])
            self.assertEqual(value["reason"], "ci_environment_required")
            self.assertIn("rss_excludes_compressed_memory", value["limitations"])
            self.assertIn("some_gpu_memory_not_measured", value["limitations"])
            original = output.read_bytes()
            with patch("builtins.print"):
                self.assertEqual(smoke.main([folder, "--report", str(output)]), 1)
            self.assertEqual(output.read_bytes(), original)
            self.assertNotIn(folder, json.dumps(value))

    def test_report_and_sample_collections_have_hard_caps(self):
        with tempfile.TemporaryFile(mode="w+") as output:
            with self.assertRaises(ValueError):
                smoke.write_report(output, {"fixture": "x" * smoke.MAX_REPORT_BYTES})
        with patch.object(smoke, "MAX_SAMPLES", 2):
            result, child, _ = self.monitor_fake([1024] * 4)
        self.assertEqual(result["reason"], "sample_limit")
        self.assertEqual(len(result["samples"]), 2)
        self.assertIsNotNone(child.poll())

    def test_two_scenarios_use_distinct_temporary_profiles_and_only_the_init_command(self):
        profiles = []
        calls = []
        def initialize(command, _limits, _stop, _env):
            calls.append(command)
            self.assertEqual(command[1:3], ["init", "--identity"])
            identity = Path(command[3])
            identity.mkdir()
            # Fixture material is not a real identity, pin or authentication.
            for name in ("node.crt.der", "node.key.der"):
                (identity / name).write_bytes(b"fixture")
            return {"reason": "identity_created", "pid": 2, "termination": "already_exited",
                    "exit_status": {"returncode": 0, "signal": None}}
        def inspect(command, _limits, *, scenario, env, stop, footprint):
            self.assertEqual(len(command), 3)
            self.assertEqual(command[1], "--data-dir")
            profile = Path(command[2])
            profiles.append(profile)
            self.assertTrue(profile.is_dir())
            self.assertEqual((profile / "identity").exists(), scenario == "existing_identity")
            self.assertEqual(Path(env["XDG_DATA_HOME"]).parent, profile.parent)
            self.assertEqual(stop.signum, 0)
            return {"scenario": scenario, "pid": 1, "passed": True}
        with patch.object(smoke, "initialize_identity", side_effect=initialize), \
                patch.object(smoke, "monitor", side_effect=inspect):
            result = smoke.run_scenarios(Path("unused-binaries"), smoke.Limits(), smoke.StopFlag())
        self.assertEqual([item["scenario"] for item in result], ["empty_identity", "existing_identity"])
        self.assertEqual(len(calls), 1)
        self.assertNotEqual(profiles[0], profiles[1])
        self.assertTrue(all(not profile.exists() for profile in profiles))

    def test_vmmap_summary_extracts_only_finite_numeric_units_and_never_paths(self):
        for unit, expected in [("K", 2), ("M", 1536), ("G", 1572864), ("T", 1610612736)]:
            fixture = ("Process: private/user/path\nPhysical footprint: 1.5" + unit
                       + "\nPhysical footprint (peak): 2" + unit + "\n").encode()
            value = smoke.parse_footprint_summary(fixture)
            self.assertEqual(value["current_kib"], expected)
            self.assertNotIn("private", json.dumps(value))
        malformed = [b"Physical footprint: 1M", b"Physical footprint: NaNM\nPhysical footprint (peak): 2M",
            b"Physical footprint: -1M\nPhysical footprint (peak): 2M",
            b"Physical footprint: 1P\nPhysical footprint (peak): 2P",
            b"Physical footprint: 1M\nPhysical footprint: 1M\nPhysical footprint (peak): 2M",
            b"private/user/path" * 20000]
        for fixture in malformed:
            with self.assertRaises(ValueError):
                smoke.parse_footprint_summary(fixture)
        with patch.object(smoke.subprocess, "check_output", return_value=
                b"Physical footprint: 100M\nPhysical footprint (peak): 120M") as query:
            self.assertEqual(smoke.sample_footprint_kib(555)["current_kib"], 102400)
            self.assertEqual(query.call_args.args[0], ["vmmap", "-summary", "555"])
            self.assertEqual(query.call_args.kwargs["timeout"], 2)

    def test_rss_cap_precedes_vmmap_and_physical_absolute_includes_reported_peak(self):
        with patch.object(smoke, "sample_footprint_kib") as footprint:
            result, child, clock = self.monitor_fake([513 * 1024], footprint=footprint)
            footprint.assert_not_called()
            self.assertEqual(clock.now, 0)
            self.assertEqual(child.terminates, 1)
            self.assertEqual(result["reason"], "rss_limit")
        result, child, clock = self.monitor_fake([1024], footprint=lambda _pid:
            {"current_kib": 100 * 1024, "reported_peak_kib": 513 * 1024})
        self.assertEqual(result["reason"], "physical_footprint_limit")
        self.assertFalse(result["passed"])
        self.assertFalse(result["physical_footprint"]["checks_passed"])
        self.assertEqual(child.terminates, 1)
        self.assertEqual(clock.now, 0)

    def test_physical_samples_at_zero_warmup_and_end_and_growth_threshold_stops(self):
        limits = smoke.Limits(duration=30, warmup=15, growth_limit_mib=64)
        calls = []
        def stable(pid):
            calls.append(pid)
            return {"current_kib": 100 * 1024, "reported_peak_kib": 512 * 1024}
        result, _child, _clock = self.monitor_fake([1024] * 31, limits=limits, footprint=stable)
        physical = result["physical_footprint"]
        self.assertEqual([item["elapsed_sec"] for item in physical["samples"]], [0, 15, 30])
        self.assertEqual(calls, [FakeChild.pid] * 3)
        self.assertTrue(physical["checks_passed"])
        self.assertEqual(physical["availability"], "available")
        self.assertEqual(physical["peak_growth_kib"], 0)
        self.assertTrue(result["rss_checks_passed"])
        for final_current, final_peak in [(165, 200), (100, 265)]:
            sequence = iter([(90, 200), (100, 200), (final_current, final_peak)])
            def growing(_pid):
                current, peak = next(sequence)
                return {"current_kib": current * 1024, "reported_peak_kib": peak * 1024}
            result, child, _clock = self.monitor_fake([1024] * 31, limits=limits, footprint=growing)
            self.assertEqual(result["reason"], "physical_footprint_growth_limit")
            self.assertEqual(result["physical_footprint"]["peak_growth_kib"], 65 * 1024)
            self.assertFalse(result["physical_footprint"]["checks_passed"])
            self.assertEqual(child.terminates, 1)

    def test_vmmap_unavailable_or_missing_warmup_never_becomes_a_physical_pass(self):
        limits = smoke.Limits(duration=30, warmup=15)
        def unavailable(_pid):
            raise subprocess.TimeoutExpired("private/user/path", 2, output=b"private material")
        result, _child, _clock = self.monitor_fake([1024] * 31, limits=limits, footprint=unavailable)
        physical = result["physical_footprint"]
        self.assertTrue(result["rss_checks_passed"])
        self.assertEqual(physical["availability"], "unavailable")
        for field in ("checks_passed", "baseline_kib", "peak_kib", "reported_peak_kib", "peak_growth_kib"):
            self.assertIsNone(physical[field])
        value = smoke.report(limits, [result, result], "completed")
        self.assertIsNone(value["physical_footprint_checks_passed"])
        self.assertTrue(value["rss_checks_passed"])
        self.assertNotEqual(value["coverage"], "rss_and_physical_footprint")
        self.assertNotIn("private", json.dumps(value))
        calls = [0]
        def partial(_pid):
            calls[0] += 1
            if calls[0] == 2:
                return unavailable(_pid)
            return {"current_kib": 1024, "reported_peak_kib": 2048}
        result, _child, _clock = self.monitor_fake([1024] * 31, limits=limits, footprint=partial)
        self.assertEqual(result["physical_footprint"]["availability"], "partial")
        self.assertIsNone(result["physical_footprint"]["checks_passed"])
        self.assertFalse(result["physical_footprint"]["warmup_baseline_available"])

    def test_identity_timeout_reaps_small_python_child(self):
        stop = smoke.StopFlag()
        result = smoke.initialize_identity([sys.executable, "-c", "import time; time.sleep(60)"],
            smoke.Limits(init_timeout=0.05, stop_timeout=1), stop, os.environ.copy())
        self.assertEqual(result["reason"], "identity_timeout")
        self.assertEqual(result["termination"], "terminated")
        self.assertIsNotNone(result["exit_status"]["returncode"])


if __name__ == "__main__":
    unittest.main()
