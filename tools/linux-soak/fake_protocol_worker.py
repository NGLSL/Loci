#!/usr/bin/env python3
"""Protocol test double only. No Engine, native filesystem or acceptance claim."""
import sys, os, json, struct
mode = sys.argv[1] if len(sys.argv) > 1 else 'normal'
pid = os.getpid()
start = int(open('/proc/self/stat').read().rsplit(')', 1)[1].split()[19])

def write(seq, op, **extra):
    reply = {'schema': 1, 'id': seq, 'op': op, 'ok': True, 'pid': pid, 'native_engine': False, 'engine_acceptance': False, **extra}
    b = json.dumps(reply).encode()
    sys.stdout.buffer.write(struct.pack('<I', len(b)) + b)
    sys.stdout.buffer.flush()
if mode == 'oversize':
    sys.stdout.buffer.write(struct.pack('<I', 65537))
    sys.stdout.buffer.flush()
else:
    write(0, 'HELLO', process_start_ticks=start)
while True:
    n = sys.stdin.buffer.read(4)
    if not n:
        break
    size = struct.unpack('<I', n)[0]
    body = sys.stdin.buffer.read(size).decode()
    fields = body.split('\t')
    seq = int(fields[0])
    op = fields[1]
    if mode == 'wrong-id':
        write(seq + 1, op)
    elif op == 'QUERY50':
        write(seq, op, paths_hex=['2f666978747572652f7265706f7274'], version=1, validated=True, complete=True)
    elif op == 'QUIT':
        write(seq, op, exiting=True)
        break
    else:
        write(seq, op)
