import ctypes, ctypes.wintypes as w, pathlib, subprocess, time, json
root=pathlib.Path(__file__).resolve().parents[1]
(root/"results-v3").mkdir(exist_ok=True)
exe=next((root/"target/stage3/release/deps").glob("live-*.exe"))
class Counters(ctypes.Structure):
    _fields_=[("cb",w.DWORD),("PageFaultCount",w.DWORD)]+[(n,ctypes.c_size_t) for n in ["PeakWorkingSetSize","WorkingSetSize","QuotaPeakPagedPoolUsage","QuotaPagedPoolUsage","QuotaPeakNonPagedPoolUsage","QuotaNonPagedPoolUsage","PagefileUsage","PeakPagefileUsage","PrivateUsage"]]
api=ctypes.WinDLL("psapi").GetProcessMemoryInfo
api.argtypes=[w.HANDLE,ctypes.POINTER(Counters),w.DWORD]; api.restype=w.BOOL
peak_ws=peak_commit=peak_private=samples=0
start=time.monotonic()
with (root/"results-v3/live-metrics.txt").open("wb") as log:
    process=subprocess.Popen([str(exe),"measure_live_fixture_1024_records_31_renames","--ignored","--nocapture","--test-threads=1"],cwd=root,stdout=log,stderr=subprocess.STDOUT)
    while True:
        c=Counters();c.cb=ctypes.sizeof(c)
        if api(w.HANDLE(int(process._handle)),ctypes.byref(c),c.cb):
            peak_ws=max(peak_ws,c.PeakWorkingSetSize);peak_commit=max(peak_commit,c.PeakPagefileUsage);peak_private=max(peak_private,c.PrivateUsage);samples+=1
        if process.poll() is not None:break
        if time.monotonic()-start>60:process.terminate();raise RuntimeError("bounded fixture benchmark exceeded 60 seconds")
        time.sleep(.005)
result=dict(exit_code=process.returncode,wall_seconds=time.monotonic()-start,peak_working_set_bytes=peak_ws,peak_commit_bytes=peak_commit,sampled_private_bytes_max=peak_private,memory_samples=samples,sample_interval_ms=5,method="Windows GetProcessMemoryInfo child process; peak counters plus sampled private bytes; excludes kernel watcher costs")
(root/"results-v3/live-memory.json").write_text(json.dumps(result,indent=2))
print(json.dumps(result));print((root/"results-v3/live-metrics.txt").read_text())
raise SystemExit(process.returncode)
