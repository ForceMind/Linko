"""Read-only Windows counters for CI-owned PIDs. No memory contents or privileges."""
from contextlib import contextmanager
from functools import lru_cache
import ctypes as C
import sys
import time

U32, I32, SIZE, HANDLE = C.c_uint32, C.c_int32, C.c_size_t, C.c_void_p
INVALID_HANDLE = C.c_void_p(-1).value


class FileTime(C.Structure):
    _fields_ = [("low", U32), ("high", U32)]

    def ticks(self):
        return (self.high << 32) | self.low


class MemoryCounters(C.Structure):
    _fields_ = [("cb", U32), ("faults", U32)] + [(name, SIZE) for name in (
        "peak_working_set", "working_set", "peak_paged_pool", "paged_pool",
        "peak_nonpaged_pool", "nonpaged_pool", "pagefile", "peak_pagefile", "private_usage")]


class ProcessEntry(C.Structure):
    _fields_ = [("size", U32), ("usage", U32), ("pid", U32), ("heap", SIZE),
                ("module", U32), ("threads", U32), ("parent", U32), ("priority", I32),
                ("flags", U32), ("name", C.c_uint16 * 260)]


class StationFlags(C.Structure):
    _fields_ = [("inherit", I32), ("reserved", I32), ("flags", U32)]


def _pid(pid):
    if type(pid) is not int or not 0 < pid <= 0xffffffff:
        raise ValueError("Windows process query unavailable")
    return pid


class Native:
    def __init__(self, kernel=None):
        if kernel is None:
            if sys.platform != "win32":
                raise ValueError("Windows process query unavailable")
            kernel = C.WinDLL("kernel32", use_last_error=True)
            signatures = {
                "OpenProcess": ([U32, I32, U32], HANDLE),
                "CloseHandle": ([HANDLE], I32),
                "K32GetProcessMemoryInfo": ([HANDLE, C.POINTER(MemoryCounters), U32], I32),
                "GetProcessTimes": ([HANDLE] + [C.POINTER(FileTime)] * 4, I32),
                "CreateToolhelp32Snapshot": ([U32, U32], HANDLE),
                "Process32FirstW": ([HANDLE, C.POINTER(ProcessEntry)], I32),
                "Process32NextW": ([HANDLE, C.POINTER(ProcessEntry)], I32),
            }
            for name, (args, result) in signatures.items():
                function = getattr(kernel, name)
                function.argtypes, function.restype = args, result
        self.kernel = kernel

    @contextmanager
    def process(self, pid):
        handle = self.kernel.OpenProcess(0x1000, False, _pid(pid))  # QUERY_LIMITED_INFORMATION only.
        if handle in (None, 0, INVALID_HANDLE):
            raise ValueError("Windows process query unavailable")
        try:
            yield handle
        finally:
            if not self.kernel.CloseHandle(handle):
                raise ValueError("Windows process handle close failed")

    def rss_kib(self, pid):
        with self.process(pid) as handle:
            value = MemoryCounters(); value.cb = C.sizeof(value)
            if not self.kernel.K32GetProcessMemoryInfo(handle, C.byref(value), value.cb):
                raise ValueError("Windows working set unavailable")
            if not 0 <= value.working_set <= (1 << 63) - 1:
                raise ValueError("Windows working set unavailable")
            return (value.working_set + 1023) // 1024

    def cpu_time_ns(self, pid):
        with self.process(pid) as handle:
            created, exited, kernel, user = (FileTime() for _ in range(4))
            if not self.kernel.GetProcessTimes(handle, C.byref(created), C.byref(exited), C.byref(kernel), C.byref(user)):
                raise ValueError("Windows CPU time unavailable")
            value = (kernel.ticks() + user.ticks()) * 100
            if not 0 <= value <= (1 << 63) - 1:
                raise ValueError("Windows CPU time unavailable")
            return value

    def parent_pid(self, pid):
        pid = _pid(pid)
        handle = self.kernel.CreateToolhelp32Snapshot(0x2, 0)  # Process metadata, never memory contents.
        if handle in (None, 0, INVALID_HANDLE):
            raise ValueError("Windows parent unavailable")
        try:
            entry = ProcessEntry(); entry.size = C.sizeof(entry)
            valid = self.kernel.Process32FirstW(handle, C.byref(entry))
            deadline = time.monotonic() + 2
            for _ in range(65536):
                if not valid or time.monotonic() > deadline:
                    break
                if entry.pid == pid:
                    return _pid(int(entry.parent))
                valid = self.kernel.Process32NextW(handle, C.byref(entry))
            raise ValueError("Windows parent unavailable")
        finally:
            if not self.kernel.CloseHandle(handle):
                raise ValueError("Windows snapshot close failed")


def environment():
    """Finite CI desktop metadata; not authentication or physical acceptance."""
    if sys.platform != "win32":
        raise ValueError("Windows environment unavailable")
    user = C.WinDLL("user32", use_last_error=True)
    user.GetProcessWindowStation.argtypes, user.GetProcessWindowStation.restype = [], HANDLE
    user.GetUserObjectInformationW.argtypes = [HANDLE, I32, C.c_void_p, U32, C.POINTER(U32)]
    user.GetUserObjectInformationW.restype = I32
    user.FindWindowW.argtypes, user.FindWindowW.restype = [C.c_wchar_p, C.c_wchar_p], HANDLE
    flags, needed = StationFlags(), U32()
    valid = user.GetUserObjectInformationW(user.GetProcessWindowStation(), 1, C.byref(flags), C.sizeof(flags), C.byref(needed))
    return {"interactive_station": bool(flags.flags & 1) if valid else None,
            "shell_tray_present": bool(user.FindWindowW("Shell_TrayWnd", None))}


@lru_cache(maxsize=1)
def native():
    return Native()
