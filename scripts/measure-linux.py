#!/usr/bin/env python3
"""Bounded synthetic query-index checks, independent of the watch/recovery prototype."""
import json, os, pathlib, resource, subprocess, time
root = pathlib.Path(__file__).resolve().parents[1]
os.chdir(root)
work = root / 'work' / 'linux-synthetic-validation'
work.mkdir(exist_ok=True)
results = root / 'results-linux'
metrics=[]
def limits():
    resource.setrlimit(resource.RLIMIT_AS, (1024**3, 1024**3))
    resource.setrlimit(resource.RLIMIT_CPU, (90, 90))
    resource.setrlimit(resource.RLIMIT_FSIZE, (512*1024**2, 512*1024**2))
    resource.setrlimit(resource.RLIMIT_NOFILE, (256, 256))
for n in (100_000,1_000_000):
    db = work / f'synthetic-{n}.idx'
    for mode, args in [('build',['build',str(n),str(db)]),('bench',['bench',str(db)])]:
        output=results / f'synthetic-{n}-{mode}.txt'
        t=time.monotonic()
        with output.open('w') as log:
            p=subprocess.Popen([str(root/'target/release/loci-experiment'),*args], stdout=log, stderr=subprocess.STDOUT, preexec_fn=limits)
            peak=0
            try:
                while p.poll() is None:
                    if time.monotonic()-t > 120:
                        p.kill();p.wait();raise TimeoutError('120-second wall budget')
                    try:
                        for line in pathlib.Path(f'/proc/{p.pid}/status').read_text().splitlines():
                            if line.startswith(('VmRSS:','VmHWM:')): peak=max(peak,int(line.split()[1]))
                    except FileNotFoundError: pass
                    time.sleep(.01)
            except BaseException:
                if p.poll() is None: p.kill();p.wait()
                raise
        m=dict(records=n, mode=mode, exit_code=p.returncode, wall_seconds=round(time.monotonic()-t,4), sampled_peak_rss_kib=peak, index_bytes=db.stat().st_size if db.exists() else None)
        metrics.append(m)
        print(json.dumps(m),flush=True)
        (results/'synthetic-metrics.json').write_text(json.dumps(metrics,indent=2)+'\n')
        if p.returncode: raise SystemExit(p.returncode)
