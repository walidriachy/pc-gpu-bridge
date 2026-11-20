"""System RAM reporting without extra dependencies (Windows, Linux/WSL, macOS)."""

from __future__ import annotations

import ctypes
import subprocess
import sys


def _windows() -> dict:
    class MEMORYSTATUSEX(ctypes.Structure):
        _fields_ = [
            ("dwLength", ctypes.c_ulong),
            ("dwMemoryLoad", ctypes.c_ulong),
            ("ullTotalPhys", ctypes.c_ulonglong),
            ("ullAvailPhys", ctypes.c_ulonglong),
            ("ullTotalPageFile", ctypes.c_ulonglong),
            ("ullAvailPageFile", ctypes.c_ulonglong),
            ("ullTotalVirtual", ctypes.c_ulonglong),
            ("ullAvailVirtual", ctypes.c_ulonglong),
            ("sullAvailExtendedVirtual", ctypes.c_ulonglong),
        ]

    s = MEMORYSTATUSEX()
    s.dwLength = ctypes.sizeof(MEMORYSTATUSEX)
    ctypes.windll.kernel32.GlobalMemoryStatusEx(ctypes.byref(s))  # type: ignore[attr-defined]
    return {"total_bytes": s.ullTotalPhys, "available_bytes": s.ullAvailPhys}


def parse_meminfo(text: str) -> dict:
    vals = {}
    for line in text.splitlines():
        k, _, v = line.partition(":")
        parts = v.split()
        if parts:
            vals[k.strip()] = int(parts[0]) * 1024
    return {"total_bytes": vals.get("MemTotal", 0), "available_bytes": vals.get("MemAvailable", 0)}


def _linux() -> dict:
    with open("/proc/meminfo", encoding="utf-8") as f:
        return parse_meminfo(f.read())


def _mac() -> dict:
    total = int(subprocess.run(["sysctl", "-n", "hw.memsize"], capture_output=True, text=True).stdout)
    vm = subprocess.run(["vm_stat"], capture_output=True, text=True).stdout
    page = 16384
    pages = {}
    for line in vm.splitlines():
        if "page size of" in line:
            page = int(line.split("page size of")[1].split()[0])
        k, _, v = line.partition(":")
        v = v.strip().rstrip(".")
        if v.isdigit():
            pages[k.strip()] = int(v)
    avail = (pages.get("Pages free", 0) + pages.get("Pages inactive", 0)
             + pages.get("Pages speculative", 0) + pages.get("Pages purgeable", 0)) * page
    return {"total_bytes": total, "available_bytes": avail}


def memory_info() -> dict:
    try:
        if sys.platform == "win32":
            m = _windows()
        elif sys.platform == "darwin":
            m = _mac()
        else:
            m = _linux()
    except Exception as exc:  # noqa: BLE001
        return {"total_bytes": 0, "available_bytes": 0, "error": str(exc)}
    total = m["total_bytes"] or 1
    m["used_percent"] = round(100 * (1 - m["available_bytes"] / total), 1)
    return m
