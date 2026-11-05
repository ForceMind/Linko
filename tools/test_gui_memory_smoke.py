"""Bounded GUI-memory supervision tests; never starts a GUI/native input."""
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time
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
    def monitor_fake(self, values, *, limits=None, child=None, stop=None):
        clock = Clock()
        child = child or FakeChild()
        samples = iter(values)
        result = smoke.monitor(["fixture"], limits or smoke.Limits(duration=4, warmup=1),
                               scenario="fixture", stop=stop, sample=lambda _pid: next(samples),
                               clock=clock, sleep=clock.sleep, popen=lambda *_a, **_k: child)
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
            with patch.dict(os.environ, {"CI": "false"}), patch.object(smoke, "run_scenarios") as run:
                self.assertEqual(smoke.main([folder, "--report", str(output)]), 1)
                run.assert_not_called()
            value = json.loads(output.read_text())
            self.assertFalse(value["passed"])
            self.assertEqual(value["reason"], "ci_environment_required")
            self.assertIn("compressed_memory_not_measured", value["limitations"])
            self.assertIn("some_gpu_memory_not_measured", value["limitations"])
            original = output.read_bytes()
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

    def test_identity_timeout_reaps_small_python_child(self):
        stop = smoke.StopFlag()
        result = smoke.initialize_identity([sys.executable, "-c", "import time; time.sleep(60)"],
            smoke.Limits(init_timeout=0.05, stop_timeout=1), stop, os.environ.copy())
        self.assertEqual(result["reason"], "identity_timeout")
        self.assertEqual(result["termination"], "terminated")
        self.assertIsNotNone(result["exit_status"]["returncode"])


if __name__ == "__main__":
    unittest.main()
