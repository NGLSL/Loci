#!/usr/bin/env python3
"""Bounded paired live-update benchmark. Results remain private local files."""
import ctypes
import json
import os
from pathlib import Path
import platform
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / "results-incremental"
if os.name != "nt":
    raise SystemExit("This benchmark requires Windows")
TARGET = "incremental"
OUT.mkdir(exist_ok=True)

from ctypes import wintypes
class Counters(ctypes.Structure):
    _fields_ = [("cb", wintypes.DWORD), ("faults", wintypes.DWORD)] + [
        (name, ctypes.c_size_t) for name in
        ["peak_ws", "ws", "peak_paged", "paged", "peak_nonpaged",
         "nonpaged", "pagefile", "peak_pagefile", "private"]]
psapi = ctypes.WinDLL("psapi")
kernel = ctypes.WinDLL("kernel32")
psapi.GetProcessMemoryInfo.argtypes = [
    wintypes.HANDLE, ctypes.POINTER(Counters), wintypes.DWORD]
kernel.GetProcessTimes.argtypes = [
    wintypes.HANDLE, ctypes.POINTER(wintypes.FILETIME),
    ctypes.POINTER(wintypes.FILETIME), ctypes.POINTER(wintypes.FILETIME),
    ctypes.POINTER(wintypes.FILETIME)]
def cpu(handle):
    stamps = [wintypes.FILETIME() for _ in range(4)]
    if not kernel.GetProcessTimes(handle, *[ctypes.byref(x) for x in stamps]):
        raise ctypes.WinError()
    seconds = [(x.dwHighDateTime * 2**32 + x.dwLowDateTime) / 1e7 for x in stamps]
    return seconds[3], seconds[2]
def memory(child):
    counters = Counters()
    counters.cb = ctypes.sizeof(counters)
    if psapi.GetProcessMemoryInfo(child._handle, ctypes.byref(counters), counters.cb):
        return {"peak_working_set_bytes": counters.peak_ws,
                "sampled_working_set_bytes": counters.ws,
                "sampled_private_bytes": counters.private,
                "peak_commit_bytes": counters.peak_pagefile}
    return {}
build = subprocess.run([
    "cargo", "+1.99.0", "test", "--release", "--offline",
    "--target-dir", "target/incremental", "--test", TARGET,
    "--no-run", "--message-format=json"], cwd=ROOT, text=True,
    stdout=subprocess.PIPE, stderr=subprocess.PIPE)
(OUT / "comparison-build.txt").write_text(build.stderr, encoding="utf-8")
if build.returncode:
    sys.stderr.write(build.stderr)
    sys.exit(build.returncode)
executables = []
for line in build.stdout.splitlines():
    item = json.loads(line)
    if item.get("reason") == "compiler-artifact" and item.get("executable") and item.get("target", {}).get("name") == TARGET:
        executables.append(item["executable"])
if len(executables) != 1:
    raise RuntimeError(f"expected one executable, found {len(executables)}")
exe = executables[0]
report = {"platform": platform.platform(), "python": platform.python_version(),
          "rust": subprocess.check_output(["rustc", "+1.99.0", "--version"], text=True).strip(),
          "target": TARGET, "sampling_interval_ms": 5, "runs": [],
          "cpu_scope": "whole child including fixture setup, assertions and cleanup",
          "latency_scope": "rename to validated query, 31 sequential samples; portable uses explicit notifications and virtual cooldown clock",
          "limits": {"files": [256, 1024, 3072], "wall_seconds": 120}}
for files in [256, 1024, 3072]:
    modes = ["rescan", "incremental"] if files != 1024 else ["incremental", "rescan"]
    for mode in modes:
        env = os.environ.copy()
        env.update(LOCI_COMPARE_FILES=str(files), LOCI_COMPARE_MODE=mode)
        log = OUT / f"compare-{files}-{mode}.txt"
        started = time.monotonic()
        peaks, samples = {}, 0
        with log.open("w", encoding="utf-8") as stream:
            child = subprocess.Popen([
                exe, "measure_incremental_comparison", "--ignored",
                "--nocapture", "--test-threads=1"], cwd=ROOT, env=env,
                stdout=stream, stderr=subprocess.STDOUT)
            try:
                while child.poll() is None:
                    for key, value in memory(child).items():
                        peaks[key] = max(peaks.get(key, 0), value)
                    samples += 1
                    if time.monotonic() - started > 120:
                        raise TimeoutError("bounded benchmark exceeded 120s")
                    time.sleep(0.005)
                child.wait()
                user, system = cpu(child._handle)
            except BaseException:
                child.kill()
                child.wait()
                raise
        lines = [x[x.index("incremental_compare,"):] for x in log.read_text(encoding="utf-8").splitlines()
                 if "incremental_compare," in x]
        if child.returncode or len(lines) != 1:
            raise RuntimeError(f"comparison failed: {log.name}, exit={child.returncode}")
        row = dict(x.split("=", 1) for x in lines[0].split(",")[1:])
        for key, value in list(row.items()):
            try:
                row[key] = float(value) if "." in value else int(value)
            except ValueError:
                pass
        row.update(peaks, memory_samples=samples, cpu_user_seconds=user,
                   cpu_system_seconds=system, cpu_total_seconds=user + system,
                   wall_seconds=time.monotonic() - started, exit_code=child.returncode)
        report["runs"].append(row)
        (OUT / "comparison-summary.json").write_text(
            json.dumps(report, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")
        print(json.dumps(row, ensure_ascii=False), flush=True)
