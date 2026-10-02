#!/usr/bin/env python3
"""Independent Python oracle for the deterministic million-record CLI fixture."""
import json, pathlib, resource, subprocess, time
root=pathlib.Path(__file__).resolve().parents[1]
queries=['invoice_00001234','ab','q','报告','报','项目','src/module_042','ext:rs','report','no_such_filename','ext:pdf report','doc report']
specs=[]
for q in queries:
    terms=q.split()
    exts=[t[4:] for t in terms if t.startswith('ext:')]
    specs.append((q,[t for t in terms if not t.startswith('ext:')],exts))
expected={q:{'count':0,'checksum':0,'first_count':0,'first_checksum':0} for q in queries}
patterns=['report_{:08}.pdf','报告_{:08}.docx','image_{:08}.png','source_{:08}.rs','invoice_{:08}.txt','ab_notes_{:08}.md','backup_{:08}.zip','日志_{:08}.log','video_{:08}.mp4','readme_{:08}.md']
for i in range(1_000_000):
    name=patterns[i%10].format(i)
    path=f'/home/test/项目/workspace_{(i//4096)%64:03}/src/module_{(i//64)%64:03}/{name}'.lower()
    ext=name.rsplit('.',1)[1]
    for q,terms,exts in specs:
        if all(t in path for t in terms) and all(e==ext for e in exts):
            v=expected[q];v['count']+=1;v['checksum']+=i+1
            if v['first_count']<50:v['first_count']+=1;v['first_checksum']+=i+1

def limits():
    resource.setrlimit(resource.RLIMIT_AS,(1024**3,1024**3))
    resource.setrlimit(resource.RLIMIT_CPU,(15,15))
rows=[]
for q in queries:
    for mode in ('complete','first50'):
        args=[str(root/'target/release/loci-experiment'),'query',str(root/'work/linux-synthetic-validation/synthetic-1000000.idx'),q]
        if mode=='complete':args.append('complete')
        start=time.monotonic()
        p=subprocess.run(args,text=True,stdout=subprocess.PIPE,stderr=subprocess.STDOUT,timeout=20,preexec_fn=limits,check=True)
        cold=next(l for l in p.stdout.splitlines() if l.startswith('cold,')).split(',')
        v=expected[q];e=(v['count'],v['checksum']) if mode=='complete' else (v['first_count'],v['first_checksum'])
        actual=(int(cold[5]),int(cold[7]))
        assert actual==e,(q,mode,actual,e)
        rows.append(dict(query=q,mode=mode,matches=actual[0],checksum=actual[1],load_us=float(cold[3]),query_us=float(cold[4]),checked=int(cold[6]),wall_seconds=round(time.monotonic()-start,4)))
        print(f'PASS {q!r} {mode}: matches={actual[0]}, checksum={actual[1]}',flush=True)
(root/'results-linux/million-oracle.json').write_text(json.dumps(rows,ensure_ascii=False,indent=2)+'\n')
print('PASS: all 24 million-record CLI results match independent count and checksum oracle')
