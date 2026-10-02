#!/usr/bin/env python3
"""Continuous native Linux soak supervisor; prep/protocol tests never establish acceptance."""
import argparse, datetime, hashlib, json, os, pathlib, queue, re, signal, stat, struct, subprocess, sys, threading, time, uuid
MAX_FRAME = 65536

class Failure(RuntimeError):
    pass

def utc():
    return datetime.datetime.now(datetime.timezone.utc).isoformat()

def digest(path):
    h = hashlib.sha256()
    with open(path, 'rb') as f:
        for chunk in iter(lambda: f.read(1024 * 1024), b''):
            h.update(chunk)
    return h.hexdigest()

def atomic_json(path, value):
    path = pathlib.Path(path)
    tmp = path.with_name(path.name + '.tmp-' + uuid.uuid4().hex)
    with open(tmp, 'x') as f:
        f.write(json.dumps(value, indent=2, ensure_ascii=True) + '\n')
        f.flush()
        os.fsync(f.fileno())
    os.replace(tmp, path)
    fd = os.open(path.parent, os.O_RDONLY | os.O_DIRECTORY)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)

def request_frame(seq, op, args):
    if not isinstance(seq, int) or seq < 1 or (not re.fullmatch('[A-Z0-9_]+', op)):
        raise Failure('invalid request identity/opcode')
    atoms = [str(seq), op, *map(str, args)]
    if any((any((c in atom for c in '\t\r\n')) or not atom.isascii() for atom in atoms)):
        raise Failure('IPC argument requires ASCII/hex encoding')
    body = '\t'.join(atoms).encode('ascii')
    if len(body) > MAX_FRAME:
        raise Failure('request frame budget exceeded')
    return struct.pack('<I', len(body)) + body

def validate_reply(reply, seq, op):
    if not isinstance(reply, dict) or reply.get('schema') != 1 or reply.get('id') != seq or (reply.get('op') != op) or (type(reply.get('ok')) is not bool) or (type(reply.get('pid')) is not int):
        raise Failure('reply schema/sequence/op/PID mismatch')
    if not reply['ok']:
        raise Failure('worker operation failed: ' + str(reply.get('error')))
    return reply

def parse_stat(text):
    try:
        f = text.rsplit(')', 1)[1].split()
        return {'state': f[0], 'cpu_ticks': int(f[11]) + int(f[12]), 'start_ticks': int(f[19])}
    except (IndexError, ValueError) as e:
        raise Failure('malformed process stat') from e

def process_sample(pid):
    p = pathlib.Path('/proc') / str(pid)
    s = parse_stat((p / 'stat').read_text())
    if s['state'] in ('Z', 'X'):
        raise Failure('bound engine process exited')
    mem = {}
    for line in (p / 'status').read_text().splitlines():
        if line.startswith(('VmRSS:', 'VmHWM:', 'VmSize:')):
            mem[line.split(':')[0].lower() + '_bytes'] = int(line.split()[1]) * 1024
    count = 0
    inotify = 0
    fds = list((p / 'fd').iterdir())
    for f in (p / 'fdinfo').iterdir():
        try:
            text = f.read_text()
        except FileNotFoundError:
            continue
        n = sum((l.startswith('inotify wd:') for l in text.splitlines()))
        count += n
        inotify += bool(n)
    return {'pid': pid, 'process_start_ticks': s['start_ticks'], 'cpu_ticks': s['cpu_ticks'], 'fd_count': len(fds), 'kernel_watch_count': count, 'inotify_fds_with_watches': inotify, 'fdinfo_available': True, 'kernel_slab_bytes': None, 'kernel_slab_reason': 'not measured by process sampler', **mem}

class BoundProcess:

    def __init__(self, pid, expected_digest):
        self.pid = pid
        self.initial = process_sample(pid)
        self.start = self.initial['process_start_ticks']
        self.binary_digest = digest(f'/proc/{pid}/exe')
        if self.binary_digest != expected_digest:
            raise Failure('engine executable digest does not match frozen worker')
        if not hasattr(os, 'pidfd_open') or not hasattr(signal, 'pidfd_send_signal'):
            raise Failure('pidfd required for safe native storm/crash signalling')
        self.exe_identity = (os.stat(f'/proc/{pid}/exe').st_dev, os.stat(f'/proc/{pid}/exe').st_ino)
        self.pidfd = os.pidfd_open(pid, 0)
        self.check()

    def check(self):
        now = process_sample(self.pid)
        if now['process_start_ticks'] != self.start:
            raise Failure('engine PID reuse/identity change')
        ex = os.stat(f'/proc/{self.pid}/exe')
        if (ex.st_dev, ex.st_ino) != self.exe_identity:
            raise Failure('bound process changed executable')
        return now

    def send(self, sig):
        self.check()
        signal.pidfd_send_signal(self.pidfd, sig)

    def close(self):
        os.close(self.pidfd)

class Logs:

    def __init__(self, root, max_bytes=256 * 1024 * 1024):
        self.root = pathlib.Path(root)
        self.root.mkdir()
        self.total = 0
        self.maximum = max_bytes
        self.lock = threading.Lock()
        self.counts = {}
        self.files = {}
        self.failed = None
        self.started = time.monotonic()

    def write(self, kind, value):
        data = (json.dumps(value, ensure_ascii=True, separators=(',', ':')) + '\n').encode() if not isinstance(value, bytes) else value
        with self.lock:
            cap = 64 * 1024 * 1024 if kind == 'events' else 16 * 1024 * 1024 if kind == 'stderr' else 160 * 1024 * 1024
            used = self.counts.get(kind, 0)
            if used + len(data) > cap or self.total + len(data) > self.maximum:
                self.failed = 'log budget exhausted'
                raise Failure(self.failed)
            bucket = used // (8 * 1024 * 1024)
            hour = int((time.monotonic() - self.started) // 3600)
            key = (kind, hour, bucket)
            if key not in self.files:
                self.files[key] = open(self.root / f'{kind}-hour{hour:02d}-{bucket:03d}.jsonl', 'xb')
            self.files[key].write(data)
            self.files[key].flush()
            self.counts[kind] = used + len(data)
            self.total += len(data)

    def close(self):
        for f in self.files.values():
            f.flush()
            os.fsync(f.fileno())
            f.close()

class Client:

    def __init__(self, argv, logs, timeout=30):
        self.logs = logs
        self.seq = 0
        self.timeout = timeout
        self.messages = queue.Queue(8)
        self.closed = False
        self.process = subprocess.Popen(argv, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, bufsize=0, start_new_session=True)
        self.transport = BoundProcess(self.process.pid, digest(f'/proc/{self.process.pid}/exe'))
        self.threads = [threading.Thread(target=self._read, daemon=True), threading.Thread(target=self._stderr, daemon=True)]
        for thread in self.threads:
            thread.start()
        try:
            self.hello = validate_reply(self._receive(timeout), 0, 'HELLO')
            if self.hello['pid'] != self.process.pid:
                raise Failure('HELLO is not the actual spawned worker PID')
        except BaseException:
            self.abort()
            raise

    def _read(self):
        try:

            def exact(n):
                b = bytearray()
                while len(b) < n:
                    part = self.process.stdout.read(n - len(b))
                    if not part:
                        raise Failure('worker IPC EOF mid-frame')
                    b.extend(part)
                return bytes(b)
            while True:
                n = struct.unpack('<I', exact(4))[0]
                if not 1 <= n <= MAX_FRAME:
                    raise Failure('response frame budget exceeded')
                self.messages.put(json.loads(exact(n).decode('utf8')), timeout=1)
        except Exception as e:
            try:
                self.messages.put(e, timeout=1)
            except queue.Full:
                pass

    def _stderr(self):
        while True:
            b = self.process.stderr.read(4096)
            if not b:
                return
            try:
                self.logs.write('stderr', {'utc': utc(), 'transport_pid': self.process.pid, 'hex': b.hex()})
            except Failure:
                continue

    def _receive(self, timeout):
        try:
            r = self.messages.get(timeout=timeout)
        except queue.Empty:
            raise Failure('worker reply timeout')
        if isinstance(r, Exception):
            raise Failure('worker protocol interrupted: ' + str(r)) from r
        return r

    def call(self, op, *args, timeout=None):
        if self.logs.failed:
            raise Failure(self.logs.failed)
        self.seq += 1
        start = time.monotonic_ns()
        self.process.stdin.write(request_frame(self.seq, op, args))
        self.process.stdin.flush()
        r = validate_reply(self._receive(timeout or self.timeout), self.seq, op)
        if r['pid'] != self.hello['pid']:
            raise Failure('worker changed PID inside session')
        logged = dict(r)
        if op == 'QUERY50':
            paths = logged.pop('paths_hex', [])
            logged.update(returned_paths=len(paths), paths_sha256=hashlib.sha256(json.dumps(paths).encode()).hexdigest())
        if op == 'STATUS':
            logged = {k: r[k] for k in ('schema', 'id', 'op', 'ok', 'pid', 'status', 'version', 'losses') if k in r}
            logged['coverage_gap_count'] = len(r.get('gaps', []))
        self.logs.write('events', {'utc': utc(), 'op': op, 'sequence': self.seq, 'roundtrip_ns': time.monotonic_ns() - start, 'reply': logged})
        return r

    def abort(self):
        if not self.closed:
            self.closed = True
            try:
                signal.pidfd_send_signal(self.transport.pidfd, signal.SIGKILL)
            except (OSError, Failure):
                pass
            try:
                self.process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                raise Failure('transport process failed to exit after interruption')
            finally:
                self.transport.close()
                self.close_pipes()

    def close_pipes(self):
        for thread in self.threads:
            thread.join(timeout=2)
        for stream in (self.process.stdin, self.process.stdout, self.process.stderr):
            stream.close()

    def finish(self):
        self.process.stdin.close()
        self.process.wait(timeout=30)
        self.closed = True
        self.transport.close()
        self.close_pipes()
        if self.process.returncode:
            raise Failure('worker exited unsuccessfully')

class RunStore:

    def __init__(self, path, sha, binary, preparation=False):
        self.path = pathlib.Path(path)
        self.path.mkdir(exist_ok=True)
        self.manifest = self.path / 'run.json'
        if self.manifest.exists():
            self.document = json.loads(self.manifest.read_text())
            if self.document.get('sha') != sha or self.document.get('binary_sha256') != binary:
                raise Failure('resume artifacts/SHA differ; create a new run directory')
            for w in self.document['windows']:
                if w['status'] == 'running':
                    try:
                        old = parse_stat(pathlib.Path(f"/proc/{w['supervisor_pid']}/stat").read_text())
                        alive = old['state'] not in ('Z', 'X') and old['start_ticks'] == w['supervisor_start_ticks'] and (w.get('boot_id') == pathlib.Path('/proc/sys/kernel/random/boot_id').read_text().strip())
                    except FileNotFoundError:
                        alive = False
                    if alive:
                        raise Failure('matching supervisor is still active')
                    w.update(status='interrupted', interrupted_utc=utc(), reason='prior supervisor disappeared; previous duration is ineligible')
        else:
            self.document = {'schema': 'loci-native-soak-v1', 'run_id': uuid.uuid4().hex, 'sha': sha, 'binary_sha256': binary, 'preparation': preparation, 'engine_acceptance': False, 'windows': [], 'created_utc': utc()}
        if self.document['preparation'] != preparation:
            raise Failure('preparation and official windows need separate run directories')
        self.document.setdefault('controller_sha256', digest(__file__))
        self.current = None
        self.save()

    def save(self):
        atomic_json(self.manifest, self.document)

    def begin_window(self, extra):
        self.current = {'window_id': uuid.uuid4().hex, 'status': 'running', 'started_utc': utc(), 'started_monotonic_ns': time.monotonic_ns(), 'controller_sha256': digest(__file__), 'supervisor_pid': os.getpid(), 'supervisor_start_ticks': parse_stat(pathlib.Path('/proc/self/stat').read_text())['start_ticks'], 'boot_id': pathlib.Path('/proc/sys/kernel/random/boot_id').read_text().strip(), 'accumulated_previous_seconds': 0, **extra}
        self.document['windows'].append(self.current)
        self.save()
        return self.current

    def finish_window(self, status, extra):
        self.current.update(status=status, finished_utc=utc(), **extra)
        self.document['engine_acceptance'] = status == 'passed' and (not self.document['preparation'])
        self.save()

def verdict(preparation, elapsed, hours, oracles, release, errors):
    if preparation:
        return 'prepared-not-accepted'
    if errors or not oracles or (not release):
        return 'failed'
    if elapsed < 86400 or hours < 24:
        return 'incomplete'
    return 'passed'

def hour_kind(h):
    return 'kernel-overflow' if h % 24 in (0, 12) else 'user-overflow' if h % 24 in (6, 18) else 'cancel-rebuild'

def restart_kind(h):
    return {5: 'graceful', 11: 'crash', 17: 'graceful'}.get(h % 24)

def decode_mount_escape(path):
    out = bytearray()
    i = 0
    while i < len(path):
        if path[i] == 92:
            chunk = path[i + 1:i + 4]
            if len(chunk) != 3 or any((c not in b'01234567' for c in chunk)):
                raise Failure('malformed mountinfo raw path')
            n = int(chunk, 8)
            if n == 0 or n > 255:
                raise Failure('invalid mount path byte')
            out.append(n)
            i += 4
        else:
            out.append(path[i])
            i += 1
    return bytes(out)

def mount_scope(root):
    with open('/proc/self/mountinfo', 'rb') as f:
        data = f.read(16 * 1024 * 1024 + 1)
    if len(data) > 16 * 1024 * 1024:
        raise Failure('mount table budget exhausted')
    mounts = []
    for line in data.splitlines():
        fields = line.split()
        if len(fields) < 10 or b'-' not in fields:
            raise Failure('unrecognized mount table')
        sep = fields.index(b'-')
        mounts.append((decode_mount_escape(fields[4]), int(fields[0]), fields[sep + 1].decode('ascii')))
    containing = [m for m in mounts if root == m[0] or root.startswith(m[0].rstrip(b'/') + b'/')]
    if not containing:
        raise Failure('selected filesystem mount unknown')
    fd = os.open(root, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        info = pathlib.Path(f'/proc/self/fdinfo/{fd}').read_text()
        mid = int(next((l.split(':', 1)[1] for l in info.splitlines() if l.startswith('mnt_id:'))))
    finally:
        os.close(fd)
    identified = [m for m in containing if m[1] == mid]
    if not identified:
        raise Failure('held root mount ID missing from independent mount table')
    selected = identified[0]
    nested = [m[0] for m in mounts if m[0] != root and m[0].startswith(root.rstrip(b'/') + b'/')]
    return {'type': selected[2], 'mount_id': selected[1], 'boundaries': nested}

def make_oracle(root, destination, boundaries, exclusions, max_bytes):
    root = os.fsencode(root)
    destination = pathlib.Path(destination)
    raw = destination.with_name(destination.name + '.unsorted')
    written = 0
    count = 0
    stack = [root]
    with open(raw, 'xb') as out:
        # Reserve the final name before walking; unknown artifacts are never overwritten.
        reserve = os.open(destination, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 384)
        os.close(reserve)
        while stack:
            parent = stack.pop()
            with os.scandir(parent) as entries:
                for entry in entries:
                    p = entry.path
                    relative = p[len(root) + 1:]
                    if any((relative == e or relative.startswith(e.rstrip(b'/') + b'/') for e in exclusions)):
                        continue
                    mode = entry.stat(follow_symlinks=False).st_mode
                    if not (stat.S_ISDIR(mode) or stat.S_ISREG(mode) or stat.S_ISLNK(mode)):
                        continue
                    payload = p + b'\x00'
                    if written + len(payload) > max_bytes:
                        raise Failure('independent oracle output budget exhausted')
                    out.write(payload)
                    written += len(payload)
                    count += 1
                    if stat.S_ISDIR(mode) and p not in boundaries:
                        stack.append(p)
        out.flush()
        os.fsync(out.fileno())
    with open(destination, 'wb') as final:
        p = subprocess.run(['sort', '-z', '-S', '64M', '-T', str(destination.parent), str(raw)], stdout=final, stderr=subprocess.PIPE, env={**os.environ, 'LC_ALL': 'C'}, timeout=600)
        if p.returncode:
            raise Failure('external raw-byte sort failed: ' + p.stderr[:4096].decode('utf8', 'replace'))
        final.flush()
        os.fsync(final.fileno())
    raw.unlink()
    return {'count': count, 'bytes': written, 'sha256': digest(destination)}

def compare_nul(a, b, max_bytes):
    if pathlib.Path(a).stat().st_size > max_bytes or pathlib.Path(b).stat().st_size > max_bytes:
        raise Failure('comparison input budget exceeded')
    with open(a, 'rb') as x, open(b, 'rb') as y:
        while True:
            p = x.read(1024 * 1024)
            q = y.read(1024 * 1024)
            if p != q:
                raise Failure('complete native raw-path sets differ')
            if not p:
                break
    previous = None
    pending = bytearray()
    count = 0
    with open(a, 'rb') as f:
        while True:
            chunk = f.read(1024 * 1024)
            if not chunk:
                break
            pending.extend(chunk)
            parts = pending.split(b'\x00')
            pending = bytearray(parts.pop())
            for path in parts:
                if path == previous or not path:
                    raise Failure('duplicate/empty path in complete export')
                previous = path
                count += 1
    if pending:
        raise Failure('unterminated NUL path stream')
    return count

class Supervisor:

    def __init__(self, cfg, run, preparation=False):
        self.cfg = cfg
        self.preparation = preparation
        self.root = os.fsencode(cfg['root'])
        self.run = pathlib.Path(run).resolve()
        self.store = RunStore(self.run, cfg['sha'], cfg['worker_binary_sha256'], preparation)
        self.window_dir = self.run / ('window-' + uuid.uuid4().hex)
        self.window_dir.mkdir()
        self.output = self.window_dir / 'outputs'
        self.output.mkdir()
        self.logs = Logs(self.window_dir / 'logs', cfg.get('log_budget_bytes', 256 * 1024 * 1024))
        self.worker = None
        self.bound = None
        self.serial = 0
        self.last_sample = 0.0
        self.leases = []
        self.owned = []
        self.errors = []
        self.completed_hours = 0
        self.cuts = 0
        self.release_verified = False
        self.interrupted = None
        self.scope = mount_scope(self.root)
        self.root_identity = (os.stat(self.root, follow_symlinks=False).st_dev, os.stat(self.root, follow_symlinks=False).st_ino)
        self.sampler_stop = threading.Event()
        self.sampler_thread = None
        self.sampler_error = None
        self.last_public = None
        self.last_public_at = None
        self.native_paused = False
        self.stable_resources = []
        self.window_start = None
        self.hz = os.sysconf('SC_CLK_TCK')
        self.phase = 'initialization'
        self.receipt = self.run / 'mutation-receipt.json'
        self.parent_relative = os.fsencode(cfg.get('mutation_parent', 'dir00000'))
        self.original_relative = self.parent_relative
        self.rename_receipt = None
        self.permission_receipt = None
        self.created = {}
        self.file_rename_receipt = None
        self.used_log_bytes = sum((f.stat().st_size for f in self.run.rglob('*.jsonl')))
        if self.used_log_bytes >= cfg.get('log_budget_bytes', 256 * 1024 * 1024):
            raise Failure('retained previous-window logs already exhaust total run log budget')
        self.logs.maximum -= self.used_log_bytes

    def start_sampler(self):
        self.sampler_stop.clear()
        self.sampler_error = None

        def run():
            while not self.sampler_stop.is_set():
                try:
                    row = self.bound.check()
                    now = time.monotonic()
                    self.check_budgets()
                    if row.get('vmhwm_bytes', 0) > self.cfg.get('peak_rss_limit_bytes', 512 * 1024 * 1024):
                        raise Failure('engine HWM exceeded512MiB during asynchronous work')
                    if self.phase == 'quiet' and row.get('vmrss_bytes', 0) > self.cfg.get('steady_rss_limit_bytes', 200 * 1024 * 1024):
                        raise Failure('quiet RSS sample exceeded200MiB')
                    self.logs.write('resources', {'utc': utc(), 'sha': self.cfg['sha'], 'binary_sha256': self.bound.binary_digest, 'phase': self.phase, 'elapsed_seconds': 0 if self.window_start is None else now - self.window_start, 'public_status_age_seconds': None if self.last_public_at is None else now - self.last_public_at, 'public_last_observation': self.last_public, 'native_worker_intentionally_paused': self.native_paused, **row})
                except Exception as exc:
                    self.sampler_error = 'resource sampling interrupted: ' + str(exc)
                    return
                self.sampler_stop.wait(self.cfg.get('sample_interval_seconds', 5))
        self.sampler_thread = threading.Thread(target=run, name='native-process-sampler', daemon=True)
        self.sampler_thread.start()

    def stop_sampler(self, check_error=True):
        self.sampler_stop.set()
        if self.sampler_thread:
            self.sampler_thread.join(timeout=10)
            if self.sampler_thread.is_alive():
                raise Failure('resource sampler failed to stop')
            self.sampler_thread = None
        if check_error and self.sampler_error:
            raise Failure(self.sampler_error)

    def check_budgets(self, reserve_bytes=0):
        maximum = self.cfg.get('artifact_budget_bytes', 4096 * 1024 * 1024)
        total = 0
        for directory, dirs, files in os.walk(self.run, followlinks=False):
            for name in dirs:
                if pathlib.Path(directory, name).is_symlink():
                    raise Failure('unexpected symlink directory in owned evidence')
            for name in files:
                f = pathlib.Path(directory, name)
                try:
                    st = f.lstat()
                except FileNotFoundError:
                    continue
                if not stat.S_ISREG(st.st_mode) or st.st_uid != os.geteuid():
                    raise Failure('unexpected unsafe result artifact')
                total += st.st_size
        if total + reserve_bytes > maximum:
            raise Failure('aggregate raw/oracle/kind/sort/checkpoint artifact budget exceeded; retain failure artifacts')
        if reserve_bytes:
            self.event('artifact-pre-admission', actual_bytes=total, reserved_bytes=reserve_bytes, aggregate_budget_bytes=maximum)
        for path in (self.root, os.fsencode(self.run)):
            v = os.statvfs(path)
            available = v.f_bavail * v.f_frsize
            if available - reserve_bytes < 64 * 1024 * 1024 or available - reserve_bytes < v.f_blocks * v.f_frsize * 0.15:
                raise Failure('filesystem byte/reserve budget exhausted')
            if v.f_files and v.f_favail < 2048:
                raise Failure('fixed inode reserve exhausted')

    def record_stable_resources(self, label):
        current = self.bound.check()
        public = self.worker.call('STATUS')
        point = {'label': label, 'pid': self.bound.pid, 'start_ticks': self.bound.start, 'native': current, 'public': public}
        self.stable_resources.append(point)
        self.event('stable-resource-point', **point)
        same = [p for p in self.stable_resources if p['pid'] == self.bound.pid and p['start_ticks'] == self.bound.start]
        if len(same) > 1:
            first = same[0]['native']
            if current['kernel_watch_count'] != first['kernel_watch_count'] or current['fd_count'] > first['fd_count'] + 2:
                raise Failure('unchanged directory topology accumulates native watches/fds')
        if len(same) >= 6:
            rss = [p['native'].get('vmrss_bytes', 0) for p in same[-6:]]
            if all((b > a for a, b in zip(rss, rss[1:]))) and rss[-1] - rss[0] > self.cfg.get('monotonic_rss_growth_limit_bytes', 8 * 1024 * 1024):
                raise Failure('six comparable stable cutoffs show sustained monotonic RSS growth; investigate before acceptance')

    def event(self, kind, **value):
        self.logs.write('events', {'utc': utc(), 'kind': kind, 'phase': self.phase, 'run_id': self.store.document['run_id'], 'sha': self.cfg['sha'], 'elapsed_seconds': 0 if self.window_start is None else time.monotonic() - self.window_start, **value})

    def check_root(self):
        s = os.stat(self.root, follow_symlinks=False)
        if not stat.S_ISDIR(s.st_mode) or (s.st_dev, s.st_ino) != self.root_identity:
            raise Failure('selected fixture root identity changed')
        scope = mount_scope(self.root)
        if scope != self.scope:
            raise Failure('selected mount scope changed during stable long-run fixture')

    def sample(self, force=False):
        if not self.bound:
            return None
        now = time.monotonic()
        if not force and now - self.last_sample < self.cfg.get('sample_interval_seconds', 5):
            return None
        self.last_sample = now
        s = self.bound.check()
        r = self.worker.call('STATUS')
        self.last_public = r
        self.last_public_at = time.monotonic()
        if s.get('vmhwm_bytes', 0) > self.cfg.get('peak_rss_limit_bytes', 512 * 1024 * 1024):
            raise Failure('engine process peak RSS budget exceeded')
        fields = r.get('resources', {})
        for used, limit in [('queued_events', 'queue_limit'), ('queued_event_bytes', 'queue_byte_limit'), ('retained_snapshot_bytes', 'retained_byte_limit'), ('snapshot_bytes', 'snapshot_byte_limit'), ('inventory_slots', 'slot_limit'), ('inventory_name_bytes', 'name_byte_limit'), ('session_watches', 'session_watch_limit'), ('process_memory_reserved_bytes', 'process_memory_limit')]:
            if not self.preparation and (type(fields.get(used)) is not int or type(fields.get(limit)) is not int):
                raise Failure('required configured resource limit unavailable: ' + limit)
            if used in fields and limit in fields and (fields[used] > fields[limit]):
                raise Failure('public resource bound exceeded: ' + used)
        self.logs.write('resources', {'utc': utc(), 'sha': self.cfg['sha'], 'binary_sha256': self.bound.binary_digest, 'phase': self.phase, 'elapsed_seconds': 0 if self.window_start is None else now - self.window_start, 'public': r, **s})
        return s

    def tick(self):
        if self.interrupted:
            raise InterruptedError(self.interrupted)
        interrupt = self.run / 'interrupt.json'
        if interrupt.exists():
            if interrupt.stat().st_size > 4096:
                raise Failure('interruption request budget exceeded')
            r = json.loads(interrupt.read_text())
            raise InterruptedError(str(r.get('reason', 'explicit supervisor interruption request')))
        if self.logs.failed:
            raise Failure(self.logs.failed)
        if self.sampler_error:
            raise Failure(self.sampler_error)
        self.check_budgets()
        self.sample()

    def wait_until(self, deadline):
        while time.monotonic() < deadline:
            self.tick()
            time.sleep(max(0, min(0.25, deadline - time.monotonic())))

    def operation(self, op, *args, deadline=None):
        if op == 'SAVE':
            self.check_budgets(self.cfg.get('export_budget_bytes', 512 * 1024 * 1024))
        r = self.worker.call(op, *args)
        if 'job_id' not in r:
            return r
        end = deadline or time.monotonic() + self.cfg.get('correction_timeout_seconds', 300)
        while time.monotonic() < end:
            self.tick()
            state = self.worker.call('OP_STATE', r['job_id'])
            v = state.get('state')
            if v == 'Complete':
                self.worker.call('OP_DROP', r['job_id'])
                return state
            if v in ('Failed', 'Cancelled'):
                self.worker.call('OP_DROP', r['job_id'])
                raise Failure(f"{op} finished {v}: {state.get('error')}")
            if v not in ('Pending', 'Running'):
                raise Failure('unknown asynchronous operation state')
            time.sleep(0.1)
        try:
            self.worker.call('CANCEL', r['job_id'])
        finally:
            raise Failure(op + ' completion deadline exceeded')

    def validated(self, deadline):
        previous = None
        while time.monotonic() < deadline:
            self.tick()
            r = self.worker.call('STATUS')
            if r.get('status') == 'Validated' and (not r.get('gaps')):
                if previous == r.get('version'):
                    return r
                previous = r.get('version')
            else:
                previous = None
            if r.get('status') == 'Stopped':
                raise Failure('owner stopped while current coverage was required')
            time.sleep(0.2)
        raise Failure('native correction did not reach gap-free Validated by deadline')

    def start_worker(self):
        self.check_root()
        argv = [s.format(root=os.fsdecode(self.root), database=self.cfg['database'], output=str(self.output), sha=self.cfg['sha']) for s in self.cfg['worker_command']]
        self.worker = Client(argv, self.logs, self.cfg.get('ipc_timeout_seconds', 30))
        h = self.worker.hello
        if h.get('native_engine') is False and (not self.preparation):
            raise Failure('protocol testdouble cannot be a native acceptance worker')
        if h.get('sha') != self.cfg['sha'] or h.get('poll_ms') != 20:
            raise Failure('worker source SHA or production poll cadence mismatch')
        self.bound = BoundProcess(h['pid'], self.cfg['worker_binary_sha256'])
        if h.get('process_start_ticks') != self.bound.start:
            raise Failure('HELLO process start ticks disagree with actual /proc')
        if not isinstance(h.get('baseline_resources'), dict):
            raise Failure('worker pre-owner resource baseline unavailable')
        self.event('worker-start', hello=h, actual_identity=self.bound.initial)
        self.store.document.setdefault('worker_segments', []).append({'pid': self.bound.pid, 'start_ticks': self.bound.start, 'binary_sha256': self.bound.binary_digest, 'started_utc': utc(), 'hello': h})
        self.store.save()
        self.last_sample = 0
        self.start_sampler()

    def stop_worker(self):
        if not self.worker:
            return
        self.stop_sampler()
        result = self.worker.call('STOP', self.cfg.get('stop_timeout_ms', 30000), 1, timeout=35)
        if result.get('joined') is not True or result.get('stopped') is not True:
            raise Failure('STOP did not prove a joined stopped owner')
        actual = self.bound.check()
        if actual.get('vmhwm_bytes', 0) > self.cfg.get('peak_rss_limit_bytes', 512 * 1024 * 1024):
            raise Failure('final engine HWM exceeds declared peak budget')
        baseline = result.get('baseline_resources', {})
        post = result.get('post_resources', {})
        for key in ('fd_count', 'kernel_watch_count'):
            if type(baseline.get(key)) is not int or baseline[key] != post.get(key) or actual[key] != baseline[key]:
                raise Failure('post-join live-process resource baseline mismatch: ' + key)
        self.event('graceful-owner-release', baseline=baseline, worker_observed=post, controller_observed=actual)
        self.release_verified = True
        self.worker.call('QUIT')
        self.worker.finish()
        self.bound.close()
        self.bound = None
        self.worker = None

    def job_export(self, lease=None):
        self.check_budgets(5 * self.cfg.get('export_budget_bytes', 512 * 1024 * 1024))
        self.serial += 1
        name = f'export-{self.serial:06d}.nul'
        target = self.output / name
        if target.exists():
            raise Failure('export target unexpectedly exists')
        args = [name.encode().hex(), '']
        if lease is not None:
            args = [lease, *args]
            r = self.operation('LEASE_EXPORT', *args)
        else:
            r = self.operation('EXPORT', *args)
        if r.get('path_hex') != os.fsencode(target).hex():
            raise Failure('worker output path differs from requested owned output')
        st = target.lstat()
        if not stat.S_ISREG(st.st_mode) or st.st_uid != os.geteuid() or st.st_size > self.cfg.get('export_budget_bytes', 512 * 1024 * 1024):
            raise Failure('unsafe/oversized worker export')
        sorted_path = target.with_name(target.name + '.sorted')
        with open(sorted_path, 'xb') as f:
            p = subprocess.run(['sort', '-z', '-S', '64M', '-T', str(self.output), str(target)], stdout=f, stderr=subprocess.PIPE, timeout=self.cfg.get('oracle_timeout_seconds', 300), env={**os.environ, 'LC_ALL': 'C'})
            if p.returncode:
                raise Failure('engine export external sort failed')
        target.unlink()
        if r.get('kinds_path_hex'):
            kinds = target.with_suffix('.kinds')
            if r['kinds_path_hex'] != os.fsencode(kinds).hex() or not stat.S_ISREG(kinds.lstat().st_mode) or kinds.stat().st_uid != os.geteuid() or (kinds.stat().st_size > self.cfg.get('export_budget_bytes', 512 * 1024 * 1024)):
                raise Failure('unsafe typed sidecar output')
            self.event('typed-sidecar-record', bytes=kinds.stat().st_size, sha256=digest(kinds))
            kinds.unlink()
        return (sorted_path, r)

    def stable_cut(self, label, retain=False):
        self.phase = 'stable-cut:' + label
        self.check_budgets()
        self.check_root()
        deadline = time.monotonic() + self.cfg.get('oracle_timeout_seconds', 300)
        before = self.validated(deadline)
        self.check_budgets(3 * self.cfg.get('export_budget_bytes', 512 * 1024 * 1024))
        self.serial += 1
        oracle = self.output / f'oracle-{self.serial:06d}.nul'
        proof = make_oracle(self.root, oracle, self.scope['boundaries'], [os.fsencode(p) for p in self.cfg.get('exclusions', [])], self.cfg.get('export_budget_bytes', 512 * 1024 * 1024))
        export, r = self.job_export()
        self.check_root()
        after = self.worker.call('STATUS')
        if r.get('complete') is not True or r.get('validated_start_finish') is not True:
            raise Failure('full current export lacked completed gap-free validation')
        if after.get('status') != 'Validated' or after.get('gaps') or before['version'] != after.get('version') or (r.get('version') != before['version']):
            raise Failure('stable cutoff did not retain one gap-free validated version')
        n = compare_nul(oracle, export, self.cfg.get('export_budget_bytes', 512 * 1024 * 1024))
        if n != proof['count'] or r.get('count') != n:
            raise Failure('full output multiplicity/count mismatch')
        self.check_budgets()
        self.cuts += 1
        self.event('full-independent-oracle-pass', label=label, version=before['version'], count=n, oracle=proof, export_sha256=digest(export))
        export.unlink()
        if not retain:
            oracle.unlink()
        return {'path': oracle, 'version': before['version'], 'count': n}

    def journal(self):
        atomic_json(self.receipt, {'schema': 1, 'run_id': self.store.document['run_id'], 'root_hex': self.root.hex(), 'root_identity': list(self.root_identity), 'created': self.created, 'rename': self.rename_receipt, 'permission': self.permission_receipt, 'file_rename': self.file_rename_receipt})

    def owned_parent(self):
        p = self.root
        for component in self.parent_relative.split(b'/'):
            if component in (b'', b'.', b'..'):
                raise Failure('unsafe mutation parent')
            p += b'/' + component
            s = os.lstat(p)
            if not stat.S_ISDIR(s.st_mode) or s.st_uid != os.geteuid():
                raise Failure('mutation parent is not an owned no-follow directory')
        return p

    def restore_receipt(self):
        if not self.receipt.exists():
            return
        r = json.loads(self.receipt.read_text())
        if r.get('run_id') != self.store.document['run_id'] or r.get('root_hex') != self.root.hex() or tuple(r.get('root_identity', ())) != self.root_identity:
            raise Failure('mutation receipt belongs to another fixture/run')
        self.created = r['created']
        self.rename_receipt = r['rename']
        self.permission_receipt = r['permission']
        self.file_rename_receipt = r.get('file_rename')
        if self.file_rename_receipt:
            x = self.file_rename_receipt
            a = bytes.fromhex(x['from'])
            b = bytes.fromhex(x['to'])
            identity = tuple(x['identity'])
            if os.path.lexists(b):
                if os.path.lexists(a) or (os.lstat(b).st_dev, os.lstat(b).st_ino) != identity:
                    raise Failure('ambiguous temporary rename receipt')
                self.created.pop(a.hex(), None)
                self.created[b.hex()] = list(identity)
            elif not os.path.lexists(a) or (os.lstat(a).st_dev, os.lstat(a).st_ino) != identity:
                raise Failure('temporary rename source disappeared')
            self.file_rename_receipt = None
        if self.permission_receipt:
            x = self.permission_receipt
            p = bytes.fromhex(x['path'])
            s = os.lstat(p)
            if (s.st_dev, s.st_ino) != tuple(x['identity']):
                raise Failure('permission recovery object changed')
            os.chmod(p, x['mode'])
            self.permission_receipt = None
        if self.rename_receipt:
            x = self.rename_receipt
            a = bytes.fromhex(x['from'])
            b = bytes.fromhex(x['to'])
            identity = tuple(x['identity'])
            if os.path.exists(b):
                if os.path.exists(a) or (os.lstat(b).st_dev, os.lstat(b).st_ino) != identity:
                    raise Failure('ambiguous recorded directory rename recovery')
                os.rename(b, a)
                self.created = {(a + p[len(b):]).hex() if (p := bytes.fromhex(k)).startswith(b + b'/') else k: v for k, v in self.created.items()}
            elif not os.path.exists(a) or (os.lstat(a).st_dev, os.lstat(a).st_ino) != identity:
                raise Failure('recorded original directory identity disappeared')
            self.rename_receipt = None
        for hexpath, identity in self.created.items():
            p = bytes.fromhex(hexpath)
            if os.path.lexists(p):
                s = os.lstat(p)
                if identity is None:
                    raise Failure('interrupted create has unproven object ownership; retain for inspection')
                if not stat.S_ISREG(s.st_mode) or (s.st_dev, s.st_ino) != tuple(identity):
                    raise Failure('owned temporary changed; refusing cleanup')
                os.unlink(p)
        self.created = {}
        self.parent_relative = self.original_relative
        self.journal()

    def create_file(self, p):
        self.created[p.hex()] = None
        self.journal()
        fd = os.open(p, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 384)
        try:
            s = os.fstat(fd)
            self.created[p.hex()] = [s.st_dev, s.st_ino]
        finally:
            os.close(fd)
        self.journal()

    def remove_file(self, p):
        s = os.lstat(p)
        if (s.st_dev, s.st_ino) != tuple(self.created.get(p.hex(), ())):
            raise Failure('mutation no longer owns temporary file')
        os.unlink(p)
        del self.created[p.hex()]
        self.journal()

    def directory_rename(self):
        if self.rename_receipt:
            x = self.rename_receipt
            a = bytes.fromhex(x['from'])
            b = bytes.fromhex(x['to'])
            s = os.lstat(b)
            if os.path.lexists(a) or (s.st_dev, s.st_ino) != tuple(x['identity']):
                raise Failure('directory rename restoration identity changed')
            os.rename(b, a)
            self.parent_relative = self.original_relative
            self.rename_receipt = None
        else:
            a = self.owned_parent()
            b = os.path.dirname(a) + b'/.loci-soak-' + self.store.document['run_id'][:12].encode() + b'-dir'
            s = os.lstat(a)
            if os.path.lexists(b):
                raise Failure('directory rename destination is not empty')
            self.rename_receipt = {'from': a.hex(), 'to': b.hex(), 'identity': [s.st_dev, s.st_ino]}
            self.journal()
            os.rename(a, b)
            self.parent_relative = b[len(self.root) + 1:]
        old = a if self.rename_receipt else b
        new = b if self.rename_receipt else a
        self.created = {(new + p[len(old):]).hex() if (p := bytes.fromhex(k)).startswith(old + b'/') else k: v for k, v in self.created.items()}
        self.journal()
        self.event('directory-rename', old_hex=old.hex(), new_hex=new.hex())

    def churn(self, deadline):
        self.phase = 'controlled-churn'
        rate = self.cfg.get('churn_operations_per_second', 2)
        operation = 0
        next_op = time.monotonic()
        next_query = next_op
        next_rename = next_op + 60
        while time.monotonic() < deadline:
            self.tick()
            now = time.monotonic()
            if now >= next_op:
                parent = self.owned_parent()
                slot = operation % 8
                a = parent + b'/.loci-soak-' + self.store.document['run_id'][:12].encode() + f'-{slot}-a'.encode()
                b = a[:-1] + b'b'
                start = time.monotonic_ns()
                if os.path.lexists(a):
                    if os.path.lexists(b):
                        raise Failure('rename mutation target unexpectedly exists')
                    s = os.lstat(a)
                    if (s.st_dev, s.st_ino) != tuple(self.created.get(a.hex(), ())):
                        raise Failure('rename temporary object changed')
                    self.file_rename_receipt = {'from': a.hex(), 'to': b.hex(), 'identity': [s.st_dev, s.st_ino]}
                    self.journal()
                    os.rename(a, b)
                    self.created[b.hex()] = self.created.pop(a.hex())
                    self.file_rename_receipt = None
                    self.journal()
                    kind = 'rename'
                elif os.path.lexists(b):
                    self.remove_file(b)
                    kind = 'delete'
                else:
                    self.create_file(a)
                    kind = 'add'
                self.event('churn-operation', operation=operation, operation_class=kind, relative_path_hex=(b if kind == 'delete' else a)[len(self.root) + 1:].hex(), syscall_and_receipt_ns=time.monotonic_ns() - start)
                operation += 1
                next_op = now + 1 / rate
            if now >= next_query:
                self.worker.call('QUERY50', self.cfg.get('query', 'report').encode().hex())
                next_query = now + 1
            if now >= next_rename:
                self.directory_rename()
                next_rename = now + 60
            time.sleep(0.02)
        if self.rename_receipt:
            self.directory_rename()
        for p in list(self.created):
            self.remove_file(bytes.fromhex(p))

    def quiet(self, deadline):
        self.phase = 'quiet'
        self.validated(time.monotonic() + 300)
        begin = self.bound.check()
        started = time.monotonic()
        self.wait_until(max(deadline, started + 600))
        end = self.bound.check()
        elapsed = time.monotonic() - started
        cpu = 100 * (end['cpu_ticks'] - begin['cpu_ticks']) / self.hz / elapsed
        if elapsed < 600 or cpu > self.cfg.get('quiet_cpu_limit_percent', 1) or end.get('vmrss_bytes', 0) > self.cfg.get('steady_rss_limit_bytes', 200 * 1024 * 1024):
            raise Failure('actual uninterrupted quiet CPU/RSS gate failed')
        self.event('quiet-window-pass', actual_seconds=elapsed, cpu_percent_one_core=cpu, begin=begin, end=end)

    def slow_reader(self, deadline):
        baseline = self.stable_cut('slow-reader-capture', True)
        h = self.worker.call('HOLD_LEASE')
        if h.get('version') != baseline['version']:
            raise Failure('held lease version differs from its independent capture oracle')
        lease = h['lease_id']
        self.leases.append(lease)
        self.churn(deadline)
        self.phase = 'slow-reader-export'
        export, r = self.job_export(lease)
        if r.get('complete') is not True or r.get('version') != baseline['version'] or compare_nul(baseline['path'], export, self.cfg.get('export_budget_bytes', 512 * 1024 * 1024)) != baseline['count']:
            raise Failure('old pinned slow reader changed after churn')
        self.worker.call('RELEASE_LEASE', lease)
        self.leases.remove(lease)
        baseline['path'].unlink()
        export.unlink()
        self.event('slow-reader-old-full-oracle-pass', version=h['version'])

    def storm(self, kind, deadline):
        self.phase = kind
        parent = self.owned_parent()
        files = [parent + b'/.loci-soak-storm-' + self.store.document['run_id'][:12].encode() + str(i).encode() for i in range(2)]
        for p in files:
            self.create_file(p)
        self.validated(deadline)
        before = self.worker.call('STATUS')
        loss = 'KernelOverflow' if kind == 'kernel-overflow' else 'UserOverflow'
        if loss in before.get('losses', []):
            raise Failure('prior cumulative loss bit prevents proving a new native storm marker')
        capacity = int(pathlib.Path('/proc/sys/fs/inotify/max_queued_events').read_text())
        events = capacity + 4096 if loss == 'KernelOverflow' else min(capacity // 2, 4096)
        if events > self.cfg.get('storm_event_budget', 65536) or events < 1024:
            raise Failure('kernel queue size outside native storm fixture budget')
        self.native_paused = True
        self.bound.send(signal.SIGSTOP)
        try:
            for i in range(events):
                os.chmod(files[i % 2], 416 if i // 2 % 2 == 0 else 384)
        finally:
            self.bound.send(signal.SIGCONT)
            self.native_paused = False
        seen = False
        while time.monotonic() < deadline:
            r = self.worker.call('STATUS')
            if loss in r.get('losses', []):
                seen = True
                break
            self.tick()
            time.sleep(0.1)
        if not seen:
            raise Failure('actual native storm loss marker not observed: ' + loss)
        self.event('actual-native-loss-observed', loss=loss, sysctl_capacity=capacity, real_attribute_operations=events, reply=r)
        self.validated(deadline)
        for p in files:
            self.remove_file(p)

    def cancel_rebuild(self, deadline):
        self.phase = 'cancel-rebuild'
        old = self.worker.call('HOLD_LEASE')
        lease = old['lease_id']
        self.leases.append(lease)
        self.operation('REBUILD', deadline=deadline)
        self.worker.call('CANCEL_CORRECTION')
        while time.monotonic() < deadline:
            status = self.worker.call('STATUS')
            if any((g.get('kind') == 'Cancelled' for g in status.get('gaps', []))):
                break
            self.tick()
            time.sleep(0.1)
        else:
            raise Failure('cancelled recovery state not observed')
        query = self.worker.call('QUERY50', self.cfg.get('query', 'report').encode().hex())
        if query.get('validated') is not False or query.get('version') != old['version']:
            raise Failure('cancelled recovery lost truthful old queries')
        self.event('cancelled-correction-old-query', old_version=old['version'], query=query)
        self.operation('REBUILD', deadline=deadline)
        self.validated(deadline)
        self.worker.call('RELEASE_LEASE', lease)
        self.leases.remove(lease)

    def permissions(self, deadline):
        self.phase = 'permission-revoke-restore'
        p = self.owned_parent()
        s = os.lstat(p)
        old = self.worker.call('STATUS')
        self.permission_receipt = {'path': p.hex(), 'identity': [s.st_dev, s.st_ino], 'mode': stat.S_IMODE(s.st_mode)}
        self.journal()
        os.chmod(p, 0)
        try:
            found = False
            while time.monotonic() < deadline:
                r = self.worker.call('STATUS')
                gaps = r.get('gaps', [])
                if r.get('status') != 'Validated' and any((g.get('errno') == 13 and g.get('path_hex') == p.hex() for g in gaps)):
                    found = True
                    break
                self.tick()
                time.sleep(0.1)
            if not found:
                raise Failure('native permission revoke lacked truthful exact coverage gap')
            q = self.worker.call('QUERY50', self.cfg.get('query', 'report').encode().hex())
            if q.get('validated') is not False or q.get('version') != old.get('version'):
                raise Failure('permission gap falsely validated/replaced retained old queries')
            self.event('native-permission-gap-old-query', status=r, query=q)
        finally:
            os.chmod(p, self.permission_receipt['mode'])
            self.permission_receipt = None
            self.journal()
        self.operation('REBUILD', deadline=deadline)
        self.validated(deadline)

    def restart(self, kind, deadline):
        self.phase = 'scheduled-' + kind + '-restart'
        self.operation('SAVE', deadline=deadline)
        if kind == 'graceful':
            self.stop_worker()
        else:
            self.stop_sampler()
            identity = self.bound.check()
            self.bound.send(signal.SIGKILL)
            self.worker.process.wait(timeout=30)
            self.event('scheduled-owner-process-crash', identity=identity)
            self.worker.closed = True
            self.worker.transport.close()
            self.bound.close()
            self.worker = None
            self.bound = None
        p = self.owned_parent() + b'/.loci-soak-offline-' + self.store.document['run_id'][:12].encode()
        self.create_file(p)
        self.start_worker()
        h = self.worker.hello
        if h.get('loaded_status') != 'Pending' or not isinstance(h.get('searchable_stale_ns'), int):
            raise Failure('restart did not expose a timed searchable stale checkpoint')
        self.event('restart-stale-first', hello=h)
        self.validated(deadline)
        self.stable_cut('restart-with-offline-add')
        self.remove_file(p)

    def run_smoke(self):
        self.restore_receipt()
        self.start_worker()
        initial = self.stable_cut('protocol-smoke-initial', True)
        if initial['count'] > 1000:
            raise Failure('protocol smoke requires a dedicated tiny fixture, never the million corpus')
        lease = self.worker.call('HOLD_LEASE')['lease_id']
        self.leases.append(lease)
        p = self.owned_parent() + b'/.loci-soak-protocol-report'
        self.create_file(p)
        self.stable_cut('protocol-smoke-real-add')
        export, r = self.job_export(lease)
        if r.get('version') != initial['version'] or compare_nul(initial['path'], export, self.cfg.get('export_budget_bytes', 512 * 1024 * 1024)) != initial['count']:
            raise Failure('protocol smoke pinned lease drift')
        self.worker.call('RELEASE_LEASE', lease)
        self.leases.remove(lease)
        export.unlink()
        initial['path'].unlink()
        self.remove_file(p)
        self.directory_rename()
        self.stable_cut('protocol-smoke-real-directory-rename')
        self.directory_rename()
        self.permissions(time.monotonic() + 10)
        self.stable_cut('protocol-smoke-permission-restored')
        self.operation('SAVE')
        self.stop_worker()
        self.store.document.update(status='protocol-smoke-passed-not-accepted', engine_acceptance=False, protocol_smoke={'controller_sha256': digest(__file__), 'source_sha': self.cfg['sha'], 'binary_sha256': self.cfg['worker_binary_sha256'], 'actual_entries': initial['count'], 'actual_filesystem': self.scope['type'], 'cuts': self.cuts, 'live_pid_resource_release': self.release_verified, 'no24h_claim': True})
        self.store.save()
        return 'protocol-smoke-passed-not-accepted'

    def run_window(self):
        self.restore_receipt()
        self.start_worker()
        initial = self.stable_cut('initial')
        if initial['count'] != self.cfg.get('baseline_entries', 1000000):
            raise Failure('actual native fixture is not the requested baseline entry count')
        self.window_start = time.monotonic()
        self.store.begin_window({'requested_seconds': 86400, 'continuous_supervisor': True, 'config_sha256': hashlib.sha256(json.dumps(self.cfg, sort_keys=True).encode()).hexdigest(), 'root_hex': self.root.hex(), 'root_identity': list(self.root_identity), 'filesystem': self.scope['type'], 'mount_id': self.scope['mount_id'], 'hardware_reference_verified': False, 'window_directory': str(self.window_dir)})
        for hour in range(24):
            start = self.window_start + hour * 3600
            if time.monotonic() > start + 5:
                raise Failure('hourly schedule missed its stable start deadline')
            self.quiet(start + 600)
            self.slow_reader(start + 1800)
            self.stable_cut(f'hour-{hour}-after-churn')
            self.operation('SAVE', deadline=start + 2100)
            self.wait_until(start + 2100)
            special = hour_kind(hour)
            if special.endswith('overflow'):
                self.storm(special, start + 2400)
            else:
                self.cancel_rebuild(start + 2400)
            self.stable_cut(f'hour-{hour}-after-{special}')
            self.wait_until(start + 2400)
            self.permissions(start + 2700)
            self.stable_cut(f'hour-{hour}-after-permission')
            self.wait_until(start + 2700)
            restart = restart_kind(hour)
            if restart:
                self.restart(restart, start + 3000)
                self.stable_cut(f'hour-{hour}-after-restart')
            self.wait_until(start + 3000)
            self.operation('SAVE', deadline=start + 3300)
            self.stable_cut(f'hour-{hour}-final')
            self.record_stable_resources(f'hour-{hour}-end')
            self.phase = 'stable-tail'
            self.wait_until(start + 3600)
            self.completed_hours += 1
            self.store.current['completed_hours'] = self.completed_hours
            self.store.current['stable_cutoffs'] = self.cuts
            self.store.save()
        self.stable_cut('final')
        self.stop_worker()
        elapsed = time.monotonic() - self.window_start
        status = verdict(self.preparation, elapsed, self.completed_hours, self.cuts >= 24, self.release_verified, self.errors)
        self.store.finish_window(status, {'actual_elapsed_seconds': elapsed, 'completed_hours': self.completed_hours, 'stable_cutoffs': self.cuts, 'graceful_release_verified': self.release_verified, 'errors': self.errors})
        return status

    def shutdown(self):
        self.stop_sampler(check_error=False)
        if self.worker:
            try:
                self.sampler_error = None
                self.stop_worker()
            except Exception as e:
                self.errors.append('shutdown: ' + str(e))
                if self.bound:
                    try:
                        self.bound.send(signal.SIGCONT)
                        self.bound.send(signal.SIGKILL)
                    except (OSError, Failure):
                        pass
                    self.bound.close()
                    self.bound = None
                self.worker.abort()
                self.worker = None
        self.logs.close()

def validate_config(cfg, run, official):
    if not re.fullmatch('[0-9a-f]{40}', cfg.get('sha', '')) or not re.fullmatch('[0-9a-f]{64}', cfg.get('worker_binary_sha256', '')):
        raise Failure('full source SHA and worker SHA256 required')
    if official and (set(cfg['sha']) == {'0'} or set(cfg['worker_binary_sha256']) == {'0'}):
        raise Failure('placeholder artifact identities cannot run acceptance')
    command = cfg.get('worker_command')
    if not isinstance(command, list) or not command or (not all((isinstance(s, str) and '\x00' not in s for s in command))):
        raise Failure('explicit worker argv array required')
    if not 0 < cfg.get('churn_operations_per_second', 2) <= 10 or not 1 <= cfg.get('sample_interval_seconds', 5) <= 10:
        raise Failure('churn/sample rate outside bounded production schedule')
    if cfg.get('baseline_entries', 1000000) < 1000000 and official:
        raise Failure('24h native gate needs a real million-entry baseline')
    root = pathlib.Path(cfg['root'])
    run = pathlib.Path(run).resolve()
    db = pathlib.Path(cfg['database'])
    if not root.is_absolute() or not db.is_absolute():
        raise Failure('absolute fixture/database paths required')
    if run == root or root in run.parents or run in root.parents:
        raise Failure('persistent evidence must be separate from the indexed fixture')
    if run != db.parent and run not in db.parents:
        raise Failure('checkpoint must stay inside the owned persistent evidence run')
    canonical_parent = db.parent.resolve()
    if db.is_symlink() or (canonical_parent != run and run not in canonical_parent.parents):
        raise Failure('checkpoint path cannot escape evidence through a symlink')
    rel = os.fsencode(cfg.get('mutation_parent', 'dir00000'))
    if rel.startswith(b'/') or any((s in (b'', b'.', b'..') for s in rel.split(b'/'))):
        raise Failure('unsafe designated mutation subtree')
    for x in cfg.get('exclusions', []):
        if pathlib.Path(x).is_absolute() or '..' in pathlib.Path(x).parts:
            raise Failure('unsafe scope exclusion')
    if not 64 * 1024 * 1024 <= cfg.get('artifact_budget_bytes', 4096 * 1024 * 1024) <= 4096 * 1024 * 1024:
        raise Failure('aggregate evidence/result/checkpoint/log budget must be64MiB..4GiB')
    if cfg.get('export_budget_bytes', 512 * 1024 * 1024) > 512 * 1024 * 1024 or cfg.get('log_budget_bytes', 256 * 1024 * 1024) > 256 * 1024 * 1024:
        raise Failure('requested output/log budget exceeds approved cap')
    for key, maximum in [('quiet_cpu_limit_percent', 1), ('steady_rss_limit_bytes', 200 * 1024 * 1024), ('peak_rss_limit_bytes', 512 * 1024 * 1024)]:
        if not 0 < cfg.get(key, maximum) <= maximum:
            raise Failure('resource gate cannot be relaxed: ' + key)
    if cfg.get('export_budget_bytes', 512 * 1024 * 1024) <= 0 or cfg.get('log_budget_bytes', 256 * 1024 * 1024) <= 0:
        raise Failure('positive output/log budgets required')
    if not official:
        return
    if os.geteuid() != 1000:
        raise Failure('native permission workload must run as ordinary UID1000')
    marker = json.loads(pathlib.Path(cfg['fixture_marker']).read_text())
    recorded = marker.get('root') or str(pathlib.Path(marker['run']) / 'data')
    if marker.get('owner') != cfg['fixture_owner'] or marker.get('uid') != 1000 or pathlib.Path(recorded).resolve() != root.resolve():
        raise Failure('fixture ownership marker/root mismatch')
    if root.resolve() != root or not stat.S_ISDIR(root.lstat().st_mode) or root.stat().st_uid != 1000:
        raise Failure('owned non-symlink canonical fixture root required')
    if mount_scope(os.fsencode(root))['type'] != 'ext4':
        raise Failure('issue17 requires an actual ext4 continuous run; other environments remain separate evidence')
    attestation = json.loads(pathlib.Path(cfg['build_manifest']).read_text())
    if attestation.get('source_sha') != cfg['sha'] or attestation.get('tracked_clean') is not True or attestation.get('worker_binary_sha256') != cfg['worker_binary_sha256']:
        raise Failure('frozen clean-build artifact attestation mismatch')
    for gate in ('15', '16'):
        proof = json.loads(pathlib.Path(cfg['prerequisite_manifests'][gate]).read_text())
        if proof.get('status') not in ('pass', 'passed') or proof.get('source_sha') != cfg['sha'] or proof.get('native_behavior') is not True:
            raise Failure('same-SHA native filesystem prerequisite not passed: ' + gate)
    persistence = json.loads(pathlib.Path(cfg['persistent_evidence_attestation']).read_text())
    if persistence.get('persistent_host_owned') is not True or persistence.get('controller_path') != str(run) or (not pathlib.Path(persistence.get('host_path', '')).is_absolute()):
        raise Failure('persistent host evidence backing unverified; ephemeral container layer cannot hold final proof')
    if not pathlib.Path(cfg['worker_binary']).is_file() or digest(cfg['worker_binary']) != cfg['worker_binary_sha256']:
        raise Failure('staged worker digest mismatch')
    atomic_json(run / 'validated-inputs.json', {'config': cfg, 'build': attestation, 'evidence_backing': persistence, 'fixture_marker': marker, 'environment': {'kernel': os.uname().release, 'arch': os.uname().machine, 'uid': os.geteuid(), 'boot_id': pathlib.Path('/proc/sys/kernel/random/boot_id').read_text().strip(), 'cpu_count': os.cpu_count(), 'root_mount': {k: v for k, v in mount_scope(os.fsencode(root)).items() if k != 'boundaries'}, 'inotify_limits': {n: pathlib.Path('/proc/sys/fs/inotify/' + n).read_text().strip() for n in ('max_queued_events', 'max_user_watches', 'max_user_instances')}, 'reference_hardware_verified': False}})

def main(argv=None):
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('action', choices=['prepare', 'protocol-smoke', 'run'])
    p.add_argument('--config', type=pathlib.Path, required=True)
    p.add_argument('--run-dir', type=pathlib.Path, required=True)
    p.add_argument('--resume', action='store_true')
    args = p.parse_args(argv)
    cfg = json.loads(args.config.read_text())
    run = args.run_dir.resolve()
    run.mkdir(parents=True, exist_ok=True)
    if args.run_dir.is_symlink() or run.stat().st_uid != os.geteuid():
        p.error('owned non-symlink persistent run directory required')
    if args.action == 'prepare':
        validate_config(cfg, run, False)
        manifest = {'schema': 'loci-soak-preparation-v1', 'status': 'prepared-not-run', 'engine_acceptance': False, 'requested_seconds': 86400, 'sha': cfg['sha'], 'worker_binary_sha256': cfg['worker_binary_sha256'], 'continuous_supervisor_required': True, 'source_controller_sha256': digest(__file__), 'config': cfg, 'notes': ['No worker/oracle/workload started.', 'Passing15/16 evidence and frozen native build are required before run.', 'Interrupted supervisor windows cannot be accumulated.']}
        atomic_json(run / 'preparation.json', manifest)
        print(json.dumps({'status': manifest['status'], 'manifest': str(run / 'preparation.json')}))
        return 0
    if (run / 'run.json').exists() and (not args.resume):
        p.error('existing manifest requires --resume; it always starts a new eligible24h window')
    validate_config(cfg, run, args.action == 'run')
    supervisor = Supervisor(cfg, run, preparation=args.action == 'protocol-smoke')
    prior = {}

    def interrupt(sig, frame):
        supervisor.interrupted = 'received ' + signal.Signals(sig).name + ' at ' + utc()
    for sig in (signal.SIGTERM, signal.SIGINT):
        prior[sig] = signal.signal(sig, interrupt)
    code = 1
    try:
        status = supervisor.run_smoke() if args.action == 'protocol-smoke' else supervisor.run_window()
        code = 0 if status in ('passed', 'protocol-smoke-passed-not-accepted') else 1
    except BaseException as exc:
        reason = str(exc)
        status = 'interrupted' if isinstance(exc, (InterruptedError, KeyboardInterrupt)) else 'failed'
        supervisor.errors.append(reason)
        try:
            supervisor.event('window-' + status, reason=reason)
        except Exception:
            pass
        # Persist interruption before cleanup so cleanup cannot erase the original failure.
        facts = {'reason': reason, 'actual_elapsed_seconds': 0 if supervisor.window_start is None else time.monotonic() - supervisor.window_start, 'completed_hours': supervisor.completed_hours, 'stable_cutoffs': supervisor.cuts, 'accumulated_previous_seconds': 0, 'errors': supervisor.errors}
        if supervisor.store.current:
            supervisor.store.finish_window(status, facts)
        else:
            supervisor.store.document.update(status='initialization-' + status, engine_acceptance=False, initialization_failure=facts)
            supervisor.store.save()
    finally:
        try:
            supervisor.shutdown()
        except Exception as exc:
            supervisor.errors.append('shutdown failure: ' + str(exc))
            code = 1
        if supervisor.errors:
            supervisor.store.document['engine_acceptance'] = False
            if supervisor.store.current:
                supervisor.store.current.update(status='interrupted' if supervisor.interrupted else 'failed', errors=supervisor.errors)
            supervisor.store.save()
        for sig, handler in prior.items():
            signal.signal(sig, handler)
    print(json.dumps({'status': supervisor.store.current.get('status') if supervisor.store.current else supervisor.store.document.get('status'), 'manifest': str(run / 'run.json'), 'engine_acceptance': supervisor.store.document['engine_acceptance']}))
    return code
if __name__ == '__main__':
    sys.exit(main())
