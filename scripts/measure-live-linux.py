#!/usr/bin/env python3
"""Existing Linux Rust only; bounded true watcher/index/query fixture measurement."""
import json, os, pathlib, resource, subprocess, time
root=pathlib.Path(__file__).resolve().parents[1]
if os.uname().sysname!="Linux" or os.uname().machine!="x86_64":
    raise SystemExit("BLOCKED: x86_64 Linux required")
os.chdir(root)
results=root/"results-linux"; results.mkdir(exist_ok=True)
# Cargo emits the authoritative executable; no guessed hashed test path.
built=subprocess.run(["cargo","+1.99.0","test","--release","--offline","--test","linux_live","--no-run","--message-format=json"],capture_output=True,text=True,timeout=120,check=True)
exe=None
for line in built.stdout.splitlines():
    item=json.loads(line)
    if item.get("reason")=="compiler-artifact" and item.get("target",{}).get("name")=="linux_live":
        exe=item.get("executable") or exe
if exe is None: raise RuntimeError("no native test executable")
def limits():
    resource.setrlimit(resource.RLIMIT_AS,(512*1024**2,512*1024**2))
    resource.setrlimit(resource.RLIMIT_CPU,(60,60))
    resource.setrlimit(resource.RLIMIT_FSIZE,(16*1024**2,16*1024**2))
    resource.setrlimit(resource.RLIMIT_NOFILE,(256,256))
start=time.monotonic(); peak=0; samples=0
with (results/"native-live-metrics.txt").open("w") as log:
    proc=subprocess.Popen([exe,"measure_native_live_1024_records_31_renames","--ignored","--nocapture","--test-threads=1"],stdout=log,stderr=subprocess.STDOUT,preexec_fn=limits)
    try:
        while proc.poll() is None:
            if time.monotonic()-start>120: raise TimeoutError("120s native fixture wall budget")
            try:
                for line in pathlib.Path(f"/proc/{proc.pid}/status").read_text().splitlines():
                    if line.startswith(("VmRSS:","VmHWM:")): peak=max(peak,int(line.split()[1])*1024)
                samples+=1
            except FileNotFoundError: pass
            time.sleep(.005)
    except BaseException:
        if proc.poll() is None: proc.kill(); proc.wait()
        raise
item=dict(exit_code=proc.returncode,wall_seconds=time.monotonic()-start,sampled_peak_rss_or_hwm_bytes=peak,memory_samples=samples,sample_interval_ms=5,method="/proc RSS/HWM; excludes kernel watch/slab objects; includes fixture creation and teardown")
(results/"native-live-memory.json").write_text(json.dumps(item,indent=2)+"\n")
print(json.dumps(item)); print((results/"native-live-metrics.txt").read_text())
raise SystemExit(proc.returncode)
