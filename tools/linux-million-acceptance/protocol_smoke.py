#!/usr/bin/env python3
"""Small native public protocol check; never million/performance acceptance."""
import argparse, hashlib, json, os, pathlib, select, struct, subprocess, tempfile, time
parser=argparse.ArgumentParser(description=__doc__)
parser.add_argument('--binary',type=pathlib.Path,required=True)
parser.add_argument('--sha',required=True)
parser.add_argument('--output-parent',type=pathlib.Path,required=True)
args=parser.parse_args()
assert args.binary.is_absolute() and args.output_parent.is_absolute()
assert len(args.sha)==40 and all(c in '0123456789abcdef' for c in args.sha)
binary=args.binary
run=pathlib.Path(tempfile.mkdtemp(prefix='loci-driver-protocol-smoke-',dir=args.output_parent))
root=run/'data';root.mkdir();out=run/'out';out.mkdir()
for i in range(8):
 d=root/f'dir{i:05d}';d.mkdir()
 for j in range(8):(d/f'file_{i:05d}_{j:05d}.txt').touch()
raw=os.fsencode(root)+b'/raw_name_\xff.txt';open(raw,'wb').close()
os.link(root/'dir00000/file_00000_00000.txt',root/'hardlink_entry.txt')
os.symlink('../data',root/'cycle_symlink')
def walk():
 result={}
 todo=[os.fsencode(root)]
 while todo:
  with os.scandir(todo.pop()) as listing:
   for e in listing:
    result[e.path]='D' if e.is_dir(follow_symlinks=False) else 'L' if e.is_symlink() else 'F'
    if result[e.path]=='D':todo.append(e.path)
 return result
def exact(stream,n):
 result=b'';deadline=time.monotonic()+10
 while len(result)<n:
  if time.monotonic()>deadline:raise TimeoutError('IPC')
  if not select.select([stream],[],[],max(.001,deadline-time.monotonic()))[0]:raise TimeoutError('IPC')
  piece=os.read(stream.fileno(),n-len(result))
  if not piece:raise EOFError('worker EOF')
  result+=piece
 return result
def reply():
 length=struct.unpack('<I',exact(child.stdout,4))[0]
 assert 0<length<=65536
 return json.loads(exact(child.stdout,length))
def req(op,*args):
 global nextid
 nextid+=1;data='\t'.join([str(nextid),op,*map(str,args)]).encode('ascii')
 child.stdin.write(struct.pack('<I',len(data))+data);child.stdin.flush()
 r=reply();assert r['id']==nextid and r['op']==op and r['ok'],r
 return r
def wait_valid():
 deadline=time.monotonic()+10
 while True:
  r=req('STATUS')
  if r['status']=='Validated' and not r['gaps']:return r
  assert r['status']!='Failed',r
  if time.monotonic()>deadline:raise TimeoutError(r)
  time.sleep(.02)
def operation(op,*args):
 job=req(op,*args)['job_id'];deadline=time.monotonic()+10
 while True:
  r=req('OP_STATE',job)
  if r['state']=='Complete':req('OP_DROP',job);return r
  assert r['state'] not in ('Failed','Cancelled'),r
  if time.monotonic()>deadline:raise TimeoutError(r)
  time.sleep(.02)
def paths(r):return {bytes.fromhex(x) for x in r['paths_hex']}
child=subprocess.Popen([str(binary),'--worker','--root',str(root),'--database',str(run/'db'),'--output',str(out),'--sha',args.sha,'--smoke'],stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=open(run/'worker.stderr.log','wb'),bufsize=0)
nextid=0
try:
 hello=reply();assert hello['op']=='HELLO' and hello['id']==0 and hello['pid']==child.pid,hello
 wait_valid();original=walk();held=req('HOLD_LEASE')['lease_id']
 new=root/'added_native_entry.txt';new.touch()
 deadline=time.monotonic()+10
 while os.fsencode(new) not in paths(req('QUERY50',os.fsencode(new.name).hex())):
  if time.monotonic()>deadline:raise TimeoutError('native add missing')
  time.sleep(.02)
 wait_valid()
 old=operation('LEASE_EXPORT',held,b'old.nul'.hex(),'')
 assert set((out/'old.nul').read_bytes().split(b'\0')[:-1])==set(original)
 assert old['count']==len(original) and old['complete'] and not old['validated_start_finish']
 req('RELEASE_LEASE',held)
 current=operation('EXPORT',b'current.nul'.hex(),'')
 expected=walk()
 assert set((out/'current.nul').read_bytes().split(b'\0')[:-1])==set(expected)
 kinds={bytes.fromhex(x.split('\t')[0]):x.split('\t')[1] for x in (out/'current.kinds').read_text().splitlines()}
 assert kinds==expected and current['count']==len(expected)
 count=operation('COUNT_START',b'file'.hex());assert count['count']==64
 sortjob=req('SORT_START','')['job_id']
 deadline=time.monotonic()+10
 while True:
  state=req('OP_STATE',sortjob)
  if state['state']=='Complete':break
  assert state['state'] not in ('Failed','Cancelled'),state
  if time.monotonic()>deadline:raise TimeoutError('sort')
  time.sleep(.02)
 sorted_paths=[];offset=0
 while True:
  page=req('SORT_PAGE',sortjob,offset,50);sorted_paths.extend(bytes.fromhex(x) for x in page['paths_hex']);offset=len(sorted_paths)
  if page['complete']:break
 assert sorted_paths==sorted(expected)
 req('OP_DROP',sortjob)
 stopped=req('STOP',10000,1)
 assert stopped['joined'] and stopped['post_resources']['kernel_watch_count']==0
 assert stopped['post_resources']['fd_count']==stopped['baseline_resources']['fd_count'],stopped
 assert len(list(pathlib.Path(f'/proc/{child.pid}/fd').iterdir()))==stopped['post_resources']['fd_count']
 req('QUIT');assert child.wait(timeout=10)==0
 # Malformed transport input must terminate the owned worker without a success
 # response; use fresh real workers rather than calling framing internals.
 for broken in [struct.pack('<I', 65537), struct.pack('<I', 4) + b'1', struct.pack('<I', 8) + b'0\tSTATUS']:
  child=subprocess.Popen([str(binary),'--worker','--root',str(root),'--database',str(run/'db'),'--output',str(out),'--sha',args.sha,'--smoke'],stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=open(run/'invalid-frame.stderr.log','ab'),bufsize=0)
  hello=reply();assert hello['op']=='HELLO'
  child.stdin.write(broken);child.stdin.close()
  assert child.wait(timeout=10)!=0, 'invalid frame accepted'
  assert child.stdout.read()==b'', 'invalid frame returned a success response'
 result={'source_sha':args.sha,'binary_sha256':hashlib.sha256(binary.read_bytes()).hexdigest(),'development_protocol_check':True,'million_acceptance':False,'entries':len(expected),'run':str(run),'held_snapshot_byte_set_equal':True,'current_snapshot_byte_kind_set_equal':True,'sort_equal':True,'same_pid_fd_watch_baseline_restored':True,'malformed_transport_rejected':True}
 (run/'result.json').write_text(json.dumps(result,indent=2)+'\n');print(json.dumps(result))
finally:
 if child.poll() is None:child.kill();child.wait()
