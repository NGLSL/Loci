#!/usr/bin/env python3
"""Owned, resumable real fixture with varied directory depth and byte-path oracle.
No engine run or acceptance claim. All mutable artifacts stay under explicit RUN.
"""
import argparse, concurrent.futures, datetime, json, os, pathlib, shutil, stat, subprocess, time
OWNER = 'loci-varied-real-fixture-v1'
FAMILIES = [('invoice','txt'),('project_report','pdf'),('source_file','rs'),('报告记录','docx'),('ab_notes','md'),('backup_file','zip'),('image_file','png'),('video_file','mp4'),('logfile_q','log'),('readme_file','md')]
QUERIES = ['a','b','q','ab','re','so','txt','pdf','rs','doc','报告','报','告','记录','invoice','report','source','notes','backup','image','video','log','md','ext:rs','ext:txt','报告 ext:docx','dir00000','dir00000 report','dir00000/dir00001','no_such_file_zzz','invoice ext:txt','report source']

def emit(value): print(json.dumps(value, ensure_ascii=False), flush=True)
def write_json(path, value):
    temp = path.with_name(path.name + '.tmp')
    with open(temp, 'w') as f: json.dump(value, f, ensure_ascii=False, indent=2); f.write('\n'); f.flush(); os.fsync(f.fileno())
    os.replace(temp, path)
def directory_paths(root, count):
    out = []
    for i in range(count):
        parent = root if i % 4 == 0 else out[i-1] if i % 4 in (1,2) else out[i-3]
        out.append(parent / f'dir{i:05d}')
    return out

def checked_marker(run):
    if run.is_symlink() or run.resolve() != run or run.stat().st_uid != os.geteuid(): raise ValueError('RUN must be an owned, non-symlink absolute directory')
    marker = run / '.loci-fixture-owner.json'
    if marker.is_symlink() or not marker.is_file(): raise ValueError('fixture owner marker missing')
    info = json.loads(marker.read_text())
    if info.get('owner') != OWNER or info.get('run') != str(run) or info.get('uid') != os.geteuid(): raise ValueError('fixture owner marker mismatch')
    return info

def preflight(path, required_inodes):
    fs = os.statvfs(path)
    available = fs.f_bavail * fs.f_frsize
    required_bytes = max(32*1024*1024, required_inodes*1536)
    if fs.f_favail < required_inodes * 1.15 or available < required_bytes or fs.f_bavail < fs.f_blocks * .15: raise ValueError('insufficient inode/data/15% filesystem reserve')
    return {'free_inodes':fs.f_favail, 'available_bytes':available, 'required_inodes':required_inodes, 'required_data_bytes':required_bytes}

def create(run, count, resume, workers):
    entries = count * 50
    if count % 4 or not 4 <= count <= 40000: raise ValueError('directory count must be a multiple of 4 in 4..40000')
    started = time.monotonic()
    if run.exists():
        if not resume: raise ValueError('RUN exists; require --resume and valid ownership')
        info = checked_marker(run)
        if info['directories'] != count: raise ValueError('resume corpus mismatch')
        present=0
        for directory, dirs, files in os.walk(run/'data', followlinks=False):
            for name in dirs+files:
                metadata=(pathlib.Path(directory)/name).lstat()
                if metadata.st_uid != os.geteuid() or not (stat.S_ISDIR(metadata.st_mode) or stat.S_ISREG(metadata.st_mode)): raise ValueError('foreign/special entry in resumed fixture')
                present+=1
        info['resume_preflight']=preflight(run,max(0,entries-present))
    else:
        if run.parent.resolve() != run.parent: raise ValueError('parent must be resolved before creation')
        budget = preflight(run.parent, entries)
        run.mkdir(mode=0o700)
        info = {'owner':OWNER, 'run':str(run), 'uid':os.geteuid(), 'directories':count, 'entries':entries, 'phase':'creating', 'created_utc':datetime.datetime.now(datetime.timezone.utc).isoformat(), 'fixture_only':True, 'preflight':budget}
        write_json(run / '.loci-fixture-owner.json', info)
        (run/'data').mkdir(mode=0o700)
    folders = directory_paths(run/'data', count)
    for folder in folders:
        try: folder.mkdir(mode=0o700)
        except FileExistsError:
            meta = folder.lstat()
            if not stat.S_ISDIR(meta.st_mode) or meta.st_uid != os.geteuid(): raise ValueError('unexpected existing fixture directory')
    def files(pair):
        i, folder = pair
        fd_dir = os.open(folder, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
        try:
            for child in range(49):
                number = i*49+child; prefix, extension = FAMILIES[number % len(FAMILIES)]
                name = f'{prefix}_{number:08d}.{extension}'
                try: fd = os.open(name, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600, dir_fd=fd_dir)
                except FileExistsError:
                    meta = os.stat(name, dir_fd=fd_dir, follow_symlinks=False)
                    if not stat.S_ISREG(meta.st_mode) or meta.st_uid != os.geteuid() or meta.st_size != 0: raise ValueError('unexpected existing fixture file')
                else: os.close(fd)
        finally: os.close(fd_dir)
        return i
    with concurrent.futures.ThreadPoolExecutor(max_workers=workers) as pool:
        for done, _ in enumerate(pool.map(files, enumerate(folders)), 1):
            if done % 1000 == 0: emit({'phase':'creating','completed_directories':done,'requested_directories':count,'elapsed_seconds':time.monotonic()-started})
    info.update(phase='created', generation_seconds=time.monotonic()-started)
    write_json(run/'.loci-fixture-owner.json', info)
    write_json(run/'queries.json', QUERIES)
    emit({'phase':'fixture-created','root':str(run/'data'),'requested_entries':entries,'generation_seconds':info['generation_seconds'],'engine_acceptance':False})

def oracle(run):
    info = checked_marker(run); root = os.fsencode(run/'data'); todo=[root]
    unsorted=run/'oracle-unsorted.nul'; sorted_out=run/'oracle-sorted.nul'; counts={'entries':0,'directories':0,'files':0,'other':0}
    path_total=0; max_path=0; min_name=None; max_name=0; depth={}
    started=time.monotonic()
    with open(unsorted,'wb') as output:
        while todo:
            directory=todo.pop()
            with os.scandir(directory) as listing:
                for entry in listing:
                    metadata=entry.stat(follow_symlinks=False)
                    if metadata.st_uid != os.geteuid(): raise ValueError('foreign entry in owned fixture')
                    relative=entry.path[len(root)+1:]; name_len=len(entry.name)
                    min_name=name_len if min_name is None else min(min_name,name_len); max_name=max(max_name,name_len)
                    path_total+=len(relative); max_path=max(max_path,len(relative)); counts['entries']+=1
                    output.write(entry.path+b'\0')
                    if stat.S_ISDIR(metadata.st_mode):
                        counts['directories']+=1; d=relative.count(b'/')+1; depth[d]=depth.get(d,0)+1; todo.append(entry.path)
                    elif stat.S_ISREG(metadata.st_mode): counts['files']+=1
                    else: counts['other']+=1
        output.flush(); os.fsync(output.fileno())
    subprocess.run(['sort','--zero-terminated','--buffer-size=64M',f'--temporary-directory={run}','--output',str(sorted_out),str(unsorted)],check=True,env={**os.environ,'LC_ALL':'C'})
    result={**counts,'directory_depth_histogram':depth,'min_basename_bytes':min_name,'max_basename_bytes':max_name,'mean_relative_path_bytes':path_total/max(counts['entries'],1),'max_relative_path_bytes':max_path,'oracle_seconds':time.monotonic()-started,'oracle_path':str(sorted_out),'oracle_bytes':sorted_out.stat().st_size,'engine_acceptance':False}
    if counts['entries']!=info['entries'] or counts['directories']!=info['directories'] or counts['files']!=info['directories']*49 or counts['other'] or min_name<8 or max_name>80 or result['mean_relative_path_bytes']>160: raise ValueError('independent fixture constraints failed: '+json.dumps(result))
    unsorted.unlink(); write_json(run/'fixture-metadata.json',result); emit(result)

p=argparse.ArgumentParser(); p.add_argument('action',choices=['create','oracle','remove']); p.add_argument('run',type=pathlib.Path); p.add_argument('--directories',type=int,default=20000); p.add_argument('--workers',type=int,choices=range(1,9),default=4); p.add_argument('--resume',action='store_true'); args=p.parse_args()
run=args.run.absolute()
if args.action=='create': create(run,args.directories,args.resume,args.workers)
elif args.action=='oracle': oracle(run)
else:
    checked_marker(run)
    allowed={'.loci-fixture-owner.json','.loci-fixture-owner.json.tmp','data','queries.json','queries.json.tmp','oracle-unsorted.nul','oracle-sorted.nul','fixture-metadata.json','fixture-metadata.json.tmp'}
    if any(child.name not in allowed for child in run.iterdir()): raise ValueError('unknown run artifacts; refuse recursive cleanup')
    shutil.rmtree(run); emit({'removed_owned_fixture':str(run)})
