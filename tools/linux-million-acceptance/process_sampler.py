#!/usr/bin/env python3
"""External engine-process resource sampler; measurement only, no acceptance claim."""
import argparse, datetime, hashlib, json, os, pathlib, time
p=argparse.ArgumentParser(); p.add_argument('--pid',type=int,required=True); p.add_argument('--sha',required=True); p.add_argument('--run-id',required=True); p.add_argument('--output',type=pathlib.Path,required=True); p.add_argument('--seconds',type=float,default=600); p.add_argument('--interval',type=float,default=5); args=p.parse_args()
if args.pid<=0 or args.seconds<=0 or not 0.1<=args.interval<=60 or args.seconds>90000: p.error('invalid measurement budget')
if (args.seconds/args.interval+2)*1024 > 256*1024*1024: p.error('projected sample log exceeds 256MiB budget')
args.output.parent.mkdir(parents=True,exist_ok=True); hz=os.sysconf('SC_CLK_TCK'); proc=pathlib.Path('/proc')/str(args.pid)
def sample():
    values=(proc/'stat').read_text().rsplit(')',1)[1].split(); identity=int(values[19]); ticks=int(values[11])+int(values[12])
    if values[0]=='Z': raise ProcessLookupError('engine process exited (zombie)')
    memory={}
    for line in (proc/'status').read_text().splitlines():
        key=line.split(':',1)[0]
        if key in ('VmRSS','VmHWM','VmSize'): memory[key.lower()+'_bytes']=int(line.split()[1])*1024
    watches=0; inotify_fds=0; available=True
    try:
        fd_count=len(list((proc/'fd').iterdir()))
        for fdinfo in (proc/'fdinfo').iterdir():
            try: lines=fdinfo.read_text().splitlines()
            except FileNotFoundError: continue
            count=sum(line.startswith('inotify wd:') for line in lines); watches+=count; inotify_fds+=bool(count)
    except PermissionError: fd_count=None; watches=None; inotify_fds=None; available=False
    return {'process_start_ticks':identity,'cpu_ticks':ticks,'fd_count':fd_count,'kernel_watch_count':watches,'inotify_fds_with_watches':inotify_fds,'fdinfo_available':available,**memory}
digest=hashlib.sha256()
with open(proc/'exe','rb') as executable:
    for chunk in iter(lambda: executable.read(1024*1024),b''): digest.update(chunk)
binary_digest=digest.hexdigest()
first=sample(); began=time.monotonic(); previous=first; previous_at=began; minimum=None; maximum=None; samples=0
try:
    with open(args.output,'x') as output:
        while True:
            current=sample(); now=time.monotonic()
            if current['process_start_ticks']!=first['process_start_ticks']: raise RuntimeError('PID was reused; measurement interrupted')
            elapsed=now-began; period=now-previous_at
            row={'run_id':args.run_id,'sha':args.sha,'pid':args.pid,'binary_sha256':binary_digest,'wall_utc':datetime.datetime.now(datetime.timezone.utc).isoformat(),'elapsed_seconds':elapsed,'cpu_percent_one_core':100*(current['cpu_ticks']-previous['cpu_ticks'])/hz/max(period,1e-9),'kernel_slab_bytes':None,'kernel_slab_reason':'not measured by this process-only sampler',**current}
            output.write(json.dumps(row)+'\n'); output.flush(); samples+=1
            rss=current.get('vmrss_bytes'); minimum=rss if minimum is None else min(minimum,rss); maximum=rss if maximum is None else max(maximum,rss)
            previous=current; previous_at=now
            if elapsed>=args.seconds: break
            time.sleep(min(args.interval,args.seconds-elapsed))
    result={'run_id':args.run_id,'sha':args.sha,'pid':args.pid,'binary_sha256':binary_digest,'requested_seconds':args.seconds,'actual_seconds':previous_at-began,'samples':samples,'cpu_percent_one_core':100*(previous['cpu_ticks']-first['cpu_ticks'])/hz/(previous_at-began),'rss_min_bytes':minimum,'rss_max_bytes':maximum,'process_hwm_bytes':previous.get('vmhwm_bytes'),'complete_requested_window':previous_at-began>=args.seconds,'engine_acceptance':False}
except (FileNotFoundError,ProcessLookupError):
    result={'run_id':args.run_id,'sha':args.sha,'pid':args.pid,'interrupted_process_exit':True,'complete_requested_window':False,'engine_acceptance':False}
summary=args.output.with_name(args.output.name+'.summary.json'); summary.write_text(json.dumps(result,indent=2)+'\n'); print(json.dumps(result))
