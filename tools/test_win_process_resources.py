"""ABI/handle/failure regressions; Windows also queries and reaps an owned child."""
import ctypes as C
import os
import subprocess
import sys
import unittest

import win_process_resources as win


class Kernel:
    def __init__(self):
        self.closed = []
        self.memory_ok = self.times_ok = True
        self.entries = [(11, 7), (123, 456)]
        self.index = 0

    def OpenProcess(self, rights, inherit, pid):
        assert rights == 0x1000 and inherit is False and pid == 123
        return 99

    def CloseHandle(self, handle):
        self.closed.append(handle)
        return 1

    def K32GetProcessMemoryInfo(self, handle, pointer, size):
        assert handle == 99 and size == C.sizeof(win.MemoryCounters)
        value = C.cast(pointer, C.POINTER(win.MemoryCounters)).contents
        assert value.cb == size
        value.working_set = 1025
        return self.memory_ok

    def GetProcessTimes(self, handle, created, exited, kernel, user):
        assert handle == 99
        value = C.cast(kernel, C.POINTER(win.FileTime)).contents
        value.high, value.low = 1, 2
        C.cast(user, C.POINTER(win.FileTime)).contents.low = 3
        return self.times_ok

    def CreateToolhelp32Snapshot(self, flags, pid):
        assert flags == 2 and pid == 0
        return 77

    def fill(self, pointer):
        value = C.cast(pointer, C.POINTER(win.ProcessEntry)).contents
        assert value.size == C.sizeof(win.ProcessEntry)
        if self.index >= len(self.entries):
            return 0
        value.pid, value.parent = self.entries[self.index]
        return 1

    def Process32FirstW(self, handle, pointer):
        assert handle == 77
        self.index = 0
        return self.fill(pointer)

    def Process32NextW(self, handle, pointer):
        assert handle == 77
        self.index += 1
        return self.fill(pointer)


class WindowsResources(unittest.TestCase):
    def test_sdk_layouts_and_counter_widths(self):
        self.assertEqual(C.sizeof(win.FileTime), 8)
        self.assertEqual(C.sizeof(win.StationFlags), 12)
        self.assertEqual(C.sizeof(win.MemoryCounters), 80 if C.sizeof(C.c_void_p) == 8 else 44)
        self.assertEqual(C.sizeof(win.ProcessEntry), 568 if C.sizeof(C.c_void_p) == 8 else 556)
        self.assertEqual(win.ProcessEntry.parent.offset, 32 if C.sizeof(C.c_void_p) == 8 else 24)

    def test_working_set_rounds_up_and_100ns_cpu_units_include_both_modes(self):
        kernel = Kernel(); query = win.Native(kernel)
        self.assertEqual(query.rss_kib(123), 2)
        self.assertEqual(query.cpu_time_ns(123), ((1 << 32) + 5) * 100)
        self.assertEqual(kernel.closed, [99, 99])

    def test_missing_counters_fail_and_still_close_the_process_handle(self):
        for method in ["rss_kib", "cpu_time_ns"]:
            kernel = Kernel(); kernel.memory_ok = kernel.times_ok = False
            with self.assertRaises(ValueError):
                getattr(win.Native(kernel), method)(123)
            self.assertEqual(kernel.closed, [99])

    def test_parent_snapshot_matches_only_requested_pid_and_closes_on_missing_pid(self):
        kernel = Kernel(); query = win.Native(kernel)
        self.assertEqual(query.parent_pid(123), 456)
        with self.assertRaises(ValueError):
            query.parent_pid(789)
        self.assertEqual(kernel.closed, [77, 77])

    def test_invalid_pids_never_open_handles(self):
        for value in [0, -1, True, "123", 1 << 32]:
            with self.assertRaises(ValueError):
                win.Native(Kernel()).rss_kib(value)

    @unittest.skipUnless(sys.platform == "win32", "actual Windows APIs require Windows")
    def test_actual_owned_child_counters_parent_and_reap(self):
        child = subprocess.Popen([sys.executable, "-c", "import sys; sys.stdin.readline()"],
                                 stdin=subprocess.PIPE, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        try:
            query = win.native()
            self.assertEqual(query.parent_pid(child.pid), os.getpid())
            self.assertGreater(query.rss_kib(child.pid), 0)
            before = query.cpu_time_ns(child.pid)
            self.assertGreaterEqual(query.cpu_time_ns(child.pid), before)
            child.stdin.write(b"stop\n"); child.stdin.flush()
            self.assertEqual(child.wait(timeout=5), 0)
        finally:
            if child.poll() is None:
                child.terminate(); child.wait(timeout=5)
            child.stdin.close()


if __name__ == "__main__":
    unittest.main()
