"""Sample the viewer and native worker used by the CI live-window probe."""

import argparse
import ctypes
import queue
import re
import subprocess
import sys
import threading
import time
from ctypes import wintypes
from pathlib import Path

SAMPLE_INTERVAL = 0.2
TIME_LIMIT = 60
PROCESS_LINE = re.compile(r"Live panel resources: viewer_pid=([0-9]+) codec_pid=([0-9]+)")
REAPED_LINE = "Live panels: both regions cleared and native fixture reaped."


class ProcessMemoryCounters(ctypes.Structure):
    _fields_ = [
        ("cb", wintypes.DWORD),
        ("page_faults", wintypes.DWORD),
        ("peak_working_set", ctypes.c_size_t),
        ("working_set", ctypes.c_size_t),
        ("quota_peak_paged_pool", ctypes.c_size_t),
        ("quota_paged_pool", ctypes.c_size_t),
        ("quota_peak_non_paged_pool", ctypes.c_size_t),
        ("quota_non_paged_pool", ctypes.c_size_t),
        ("pagefile", ctypes.c_size_t),
        ("peak_pagefile", ctypes.c_size_t),
    ]


class WindowsRss:
    def __init__(self):
        self.api = ctypes.WinDLL("kernel32", use_last_error=True)
        self.api.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
        self.api.OpenProcess.restype = wintypes.HANDLE
        self.api.CloseHandle.argtypes = [wintypes.HANDLE]
        self.api.CloseHandle.restype = wintypes.BOOL
        self.api.GetExitCodeProcess.argtypes = [wintypes.HANDLE, ctypes.POINTER(wintypes.DWORD)]
        self.api.GetExitCodeProcess.restype = wintypes.BOOL
        self.api.K32GetProcessMemoryInfo.argtypes = [
            wintypes.HANDLE,
            ctypes.POINTER(ProcessMemoryCounters),
            wintypes.DWORD,
        ]
        self.api.K32GetProcessMemoryInfo.restype = wintypes.BOOL
        self.handles = {}

    def _running(self, handle):
        code = wintypes.DWORD()
        if not self.api.GetExitCodeProcess(handle, ctypes.byref(code)):
            raise ctypes.WinError(ctypes.get_last_error())
        return code.value == 259  # STILL_ACTIVE

    def sample(self, pids):
        values = {}
        for role, pid in pids.items():
            handle = self.handles.get(pid)
            if handle is None:
                handle = self.api.OpenProcess(0x1000, False, pid)  # PROCESS_QUERY_LIMITED_INFORMATION
                if not handle:
                    error = ctypes.get_last_error()
                    if error == 87:  # ERROR_INVALID_PARAMETER when the process no longer exists
                        continue
                    raise ctypes.WinError(error)
                self.handles[pid] = handle
            if self._running(handle):
                counters = ProcessMemoryCounters()
                counters.cb = ctypes.sizeof(counters)
                if not self.api.K32GetProcessMemoryInfo(handle, ctypes.byref(counters), counters.cb):
                    error = ctypes.get_last_error()
                    if self._running(handle):
                        raise ctypes.WinError(error)
                values[role] = counters.working_set
        return values

    def close(self):
        for handle in self.handles.values():
            self.api.CloseHandle(handle)


def mac_rss(pids):
    roles = {pid: role for role, pid in pids.items()}
    command = ["/bin/ps", "-o", "pid=", "-o", "ppid=", "-o", "rss="]
    for pid in roles:
        command.extend(("-p", str(pid)))
    result = subprocess.run(command, capture_output=True, text=True, timeout=2)
    if result.returncode != 0 and (result.returncode != 1 or result.stderr.strip()):
        raise RuntimeError(f"ps failed with exit {result.returncode}: {result.stderr.strip()}")
    values = {}
    for line in result.stdout.splitlines():
        pid, parent, rss_kib = map(int, line.split())
        role = roles[pid]
        if role == "codec" and parent != pids["viewer"]:
            raise RuntimeError("the reported codec PID is no longer a child of the viewer")
        values[role] = rss_kib * 1024
    return values


def collect_output(pipe, messages):
    try:
        for line in pipe:
            messages.put(line.rstrip())
    except OSError as error:
        messages.put(error)
    finally:
        messages.put(None)


def measure(process, sample):
    messages = queue.Queue()
    threading.Thread(target=collect_output, args=(process.stdout, messages), daemon=True).start()
    pids = {"viewer": process.pid}
    samples = {"viewer": [], "codec": []}
    error = None
    reaped = False
    start = time.monotonic()
    deadline = start + TIME_LIMIT
    next_sample = start
    while True:
        now = time.monotonic()
        if now >= deadline:
            raise TimeoutError("the live window probe did not finish")
        if now >= next_sample:
            if error is None:
                targets = {"viewer": process.pid} if reaped else pids
                try:
                    for role, rss in sample(targets).items():
                        if rss > 0:
                            samples[role].append((now, rss))
                except (KeyError, OSError, RuntimeError, ValueError, subprocess.TimeoutExpired) as issue:
                    error = issue
            next_sample = now + SAMPLE_INTERVAL
        try:
            message = messages.get(timeout=max(0, min(next_sample, deadline) - time.monotonic()))
        except queue.Empty:
            continue
        if message is None:
            break
        if isinstance(message, OSError):
            error = error or message
            continue
        print(message, flush=True)
        match = PROCESS_LINE.fullmatch(message)
        if match:
            viewer, codec = map(int, match.groups())
            if viewer != process.pid or codec == viewer or codec == 0 or "codec" in pids:
                error = error or RuntimeError("invalid live probe process IDs")
            else:
                pids["codec"] = codec
                next_sample = time.monotonic()
        if message == REAPED_LINE:
            reaped = True
    status = process.wait(timeout=max(1, deadline - time.monotonic()))
    if status != 0:
        print(f"Live panel memory: viewer failed with exit {status}.", file=sys.stderr)
        return 1
    if error is not None:
        raise RuntimeError(f"resident memory sampling failed: {error}") from error
    if not reaped:
        raise RuntimeError("resident memory run is missing the verified viewer exit marker")
    for role, observed in samples.items():
        if len(observed) < 2:
            raise RuntimeError(f"fewer than two live resident memory samples for {role}")
        first_time, first_rss = observed[0]
        peak = max(rss for _, rss in observed)
        span_ms = round((observed[-1][0] - first_time) * 1000)
        print(
            f"Live panel memory: {role} first_rss_bytes={first_rss} sampled_peak_rss_bytes={peak} "
            f"samples={len(observed)} span_ms={span_ms}",
            flush=True,
        )
    return 0


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("viewer", type=Path)
    parser.add_argument("fixture", type=Path)
    args = parser.parse_args()
    if sys.platform not in ("win32", "darwin"):
        parser.error("resident memory sampling requires Windows or macOS")
    windows = WindowsRss() if sys.platform == "win32" else None
    try:
        with subprocess.Popen(
            [args.viewer.resolve(), "--live-window-probe", args.fixture.resolve()],
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            encoding="utf-8",
            errors="replace",
        ) as process:
            try:
                return measure(process, windows.sample if windows else mac_rss)
            finally:
                if process.poll() is None:
                    process.kill()
    finally:
        if windows:
            windows.close()


if __name__ == "__main__":
    sys.exit(main())
