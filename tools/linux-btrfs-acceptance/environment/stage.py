#!/usr/bin/env python3
"""External source-only Btrfs glue; assembly/run require final frozen build receipt."""
import argparse, gzip, hashlib, importlib.util, json, os, pathlib, re, shlex, shutil, stat, subprocess, time
HERE = pathlib.Path(__file__).resolve().parent
SAFE_PATH = re.compile('/[A-Za-z0-9_./-]+\\Z')

def digest(path):
    h = hashlib.sha256()
    with open(path, 'rb') as f:
        for data in iter(lambda: f.read(1024 * 1024), b''):
            h.update(data)
    return h.hexdigest()

def save(path, value):
    with open(path, 'x') as f:
        json.dump(value, f, indent=2)
        f.write('\n')

def guest_destination(value):
    if not SAFE_PATH.fullmatch(value) or '..' in pathlib.PurePosixPath(value).parts or str(pathlib.PurePosixPath(value)) != value:
        raise ValueError('guest mapping requires an absolute restricted path alphabet: ' + value)
    if value in ('/init', '/proc', '/sys', '/dev') or value.startswith(('/proc/', '/sys/', '/dev/', '/artifacts/')):
        raise ValueError('artifact mapping collides with boot/system namespace: ' + value)
    return value

def helpers(source):
    spec = importlib.util.spec_from_file_location('shared_native_tools', source / 'tools/linux-native-acceptance/runner.py')
    m = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(m)
    return m

def inputs(args):
    manifest = json.loads(args.build_manifest.read_text())
    m = helpers(args.source)
    m.source_identity(args.source, args.sha)
    m.verify_manifest(manifest)
    if manifest['source']['sha'] != args.sha or manifest['profiles'] != ['debug', 'release'] or (not manifest.get('commands')) or (not manifest.get('rustc_version')):
        raise ValueError('clean exact-SHA full debug/release compiler receipt required')
    for name, want in manifest['build_log_digests'].items():
        if pathlib.Path(name).name != name or digest(args.build_manifest.parent / name) != want:
            raise ValueError('compile log mismatch')
    if not re.fullmatch('[A-Za-z0-9_-]{1,64}', args.run_id):
        raise ValueError('unsafe run token')
    overlays = manifest['runtime_overlay']
    if '/usr/bin/sort' not in [x['container_path'] for x in overlays]:
        raise ValueError('reuse shared GNU sort stage_tools receipt; no private resolver')
    entries = {}
    for row in manifest['test_artifacts'] + manifest['cli_artifacts'] + manifest['drivers']:
        dst = guest_destination(row['path'])
        if dst.startswith('/guest/'):
            raise ValueError('compiled artifact collides with reserved environment namespace')
        if dst in entries and entries[dst]['sha256'] != row['sha256']:
            raise ValueError('conflicting absolute destinations')
        entries[dst] = {'host_path': row['path'], 'sha256': row['sha256']}
    for row in overlays:
        dst = guest_destination(row['container_path'])
        entries[dst] = {'host_path': row['snapshot'], 'sha256': row['sha256']}
    cli = [r for r in manifest['cli_artifacts'] if r['profile'] == 'release']
    driver = [r for r in manifest['drivers'] if r['profile'] == 'release']
    if len(cli) != 1 or len(driver) != 1:
        raise ValueError('one final release CLI and real driver required')
    return (manifest, m, entries, cli[0]['path'], driver[0]['path'])

def source_only(args):
    receipt = {'schema': 'loci.btrfs-external-glue-preparation.v1', 'status': 'source_only_not_assembled', 'final_sha': None, 'engine_acceptance': False, 'issue16_resolved': False, 'actual_guest_execution': False, 'helpers': {p.name: digest(p) for p in HERE.iterdir() if p.is_file() and p.name != 'preparation.json'}, 'guest_hardware': {'accelerator': 'TCG', 'vcpus': 2, 'memory_mib': 2048, 'reference_ssd': 'unverified'}, 'pending': ['final frozen clean debug/release shared build manifest', 'small environment helper compile and digest receipts', 'fresh owned image provisioning', 'actual100k physical DUP capacity pilot', 'real native positive-proof cases/full1M independent oracle', 'read-only guest result extraction and host classification']}
    save(args.output, receipt)
    print(json.dumps(receipt, indent=2))

def plan(args):
    prefix=args.owned_prefix.resolve(strict=True)
    if prefix.stat().st_uid != os.geteuid() or prefix.stat().st_mode & 0o022:
        raise ValueError('owned-prefix must be user-owned and not group/other writable')
    for candidate in (args.output,args.qemu,args.kernel,args.base_initramfs,args.fs_tools):
        candidate.resolve().relative_to(prefix)
        if any(p.is_symlink() for p in (candidate,*candidate.parents)):
            raise ValueError('symlink environment input/output ancestor unsupported')
    manifest, m, entries, cli, driver = inputs(args)
    total = sum((pathlib.Path(v['host_path']).stat().st_size for v in entries.values()))
    artifact_size = max(128 * 1024 * 1024, (total * 5 // 4 + 64 * 1024 * 1024 + 1024 * 1024 - 1) // (1024 * 1024) * (1024 * 1024))
    data_size = 8 * 1024 * 1024 * 1024
    result_size = 2 * 1024 * 1024 * 1024
    v = os.statvfs(args.output.parent)
    available = v.f_bavail * v.f_frsize
    host_budget = data_size + artifact_size + 128 * 1024 * 1024
    if available - host_budget < v.f_blocks * v.f_frsize * 0.15:
        raise ValueError('host cannot retain15% after full sparse/image/log growth')
    for path in (args.source,args.build_manifest,args.qemu,args.kernel,args.base_initramfs,args.fs_tools,args.output):
        if not path.is_absolute():
            raise ValueError('explicit absolute environment/source/output paths required')
    for path in (args.base_initramfs, args.fs_tools):
        if not path.is_dir() or path.is_symlink():
            raise ValueError('explicit regular private environment directory required')
    for path in (args.qemu, args.kernel):
        if not path.is_file() or path.is_symlink():
            raise ValueError('explicit regular immutable QEMU/kernel required')
    jobs = m.jobs_for(manifest, ('suites', '100k', 'overflow'), 600)
    value = {'status': 'planned_not_assembled', 'source_sha': args.sha, 'build_manifest': str(args.build_manifest), 'build_manifest_sha256': digest(args.build_manifest), 'run_id': args.run_id, 'cli': cli, 'driver': driver, 'guest_mapping': entries, 'jobs': jobs, 'artifact_image_bytes': artifact_size, 'data_image_bytes': data_size, 'result_budget_bytes': result_size, 'host_planned_write_bytes': host_budget, 'reference_hardware_verified': False, 'engine_acceptance': False, 'issue16_resolved': False}
    if not args.assemble:
        m.source_identity(args.source,args.sha)
        save(args.output, value)
        print(json.dumps(value, indent=2))
        return
    assemble(args, value, manifest, m)

def archive_initramfs(tree, path):
    inode = 1
    with gzip.open(path, 'xb', compresslevel=1) as out:

        def entry(name, mode, data=b'', major=0, minor=0):
            nonlocal inode
            name = name.encode() + b'\x00'
            fields = [inode, mode, 0, 0, 1, 0, len(data), 0, 0, major, minor, len(name), 0]
            inode += 1
            header = b'070701' + b''.join((f'{n:08x}'.encode() for n in fields))
            out.write(header + name)
            out.write(b'\x00' * ((-len(header) - len(name)) % 4))
            out.write(data)
            out.write(b'\x00' * (-len(data) % 4))
        for p in sorted(tree.rglob('*')):
            s = p.lstat()
            data = os.readlink(p).encode() if p.is_symlink() else p.read_bytes() if p.is_file() else b''
            entry(str(p.relative_to(tree)), s.st_mode, data)
        entry('dev/console', stat.S_IFCHR | 384, major=5, minor=1)
        entry('dev/null', stat.S_IFCHR | 438, major=1, minor=3)
        entry('TRAILER!!!', 0)

def assemble(args, value, manifest, m):
    out = args.output
    if out.exists():
        raise ValueError('fresh owned output directory required; never replace previous images/evidence')
    out.mkdir(mode=448)
    art = out / 'artifact-root'
    art.mkdir()
    tree = out / 'initramfs'
    shutil.copytree(args.base_initramfs, tree, symlinks=True)
    old_alias = tree / 'workspace'
    if old_alias.is_symlink():
        old_alias.unlink()
    commands = []
    with open(out / 'environment-build.log', 'x') as log:

        def command(argv, env=None):
            commands.append(argv)
            subprocess.run(argv, check=True, env=env, stdout=log, stderr=subprocess.STDOUT)
        env = os.environ.copy()
        command([args.cc, '--version'])
        version = [args.rustc, '--version', '--verbose']
        if args.rust_env:
            version = ['bash', '-c', 'source ' + shlex.quote(str(args.rust_env)) + '; exec ' + shlex.join(version)]
        command(version)
        for src, name in [('native-probe.c', 'native-probe'), ('fixture.c', 'fixture')]:
            command([args.cc, '-O2', '-Wall', '-Wextra', str(HERE / src), '-o', str(out / name)])
        rust = [args.rustc, '-O', str(HERE / 'btrfs-matrix.rs'), '-o', str(out / 'btrfs-matrix')]
        if args.rust_env:
            rust = ['bash', '-c', 'source ' + shlex.quote(str(args.rust_env)) + '; exec ' + shlex.join(rust)]
        command(rust)
        extra = {'/guest/native-probe': out / 'native-probe', '/guest/fixture': out / 'fixture', '/guest/btrfs-matrix': out / 'btrfs-matrix', '/guest/btrfs': args.fs_tools / 'usr/bin/btrfs', '/guest/usage_guard.awk': HERE / 'usage_guard.awk', '/guest/queries.txt': args.source / 'tools/linux-million-acceptance/queries.txt'}
        for name in ('native-probe.c', 'fixture.c', 'btrfs-matrix.rs', 'init.in', 'orchestrate.in', 'workload.in', 'stage.py'):
            extra['/guest/source/' + name] = HERE / name
        for dst, host in extra.items():
            value['guest_mapping'][dst] = {'host_path': str(host), 'sha256': digest(host)}
        proof = out / 'readonly-proof'
        proof.write_bytes(b'readonly proof\n')
        proof.chmod(438)
        value['guest_mapping']['/guest/readonly-proof'] = {'host_path': str(proof), 'sha256': digest(proof)}
        jobs = []
        root_jobs = []
        for index,j in enumerate(value['jobs']):
            line = 'run_job ' + shlex.quote(j['id']) + ' ' + str(j['timeout']) + ' ' + shlex.join(j['argv'])
            logfile = '"$RESULTS/' + j['id'] + '.log"'
            line += '\ngrep -q ' + shlex.quote('test result: ok.') + ' ' + logfile + ' || { echo LOCI_BTRFS_FAIL missing_native_summary; exit 1; }'
            if j.get('required_one_test'):
                line += '\ngrep -q ' + shlex.quote('test result: ok. 1 passed; 0 failed; 0 ignored;') + ' ' + logfile + ' || { echo LOCI_BTRFS_FAIL wrong_exact_test; exit 1; }'
            if j.get('required_marker'):
                line += '\ngrep -Fq ' + shlex.quote(j['required_marker']) + ' ' + logfile + ' || { echo LOCI_BTRFS_FAIL missing_positive_native_marker; exit 1; }'
            jobs.append(str(index) + ')\n' + line + '\n;;')
            root_jobs.append('root_native_job ' + shlex.quote(j['id']) + ' ' + str(index))
        for name in ('orchestrate', 'workload'):
            body = (HERE / (name + '.in')).read_text().replace('@RESULT_BUDGET@', str(value['result_budget_bytes'])).replace('@RESULT_MIB@', '2048').replace('@NATIVE_JOB_CASES@', '\n'.join(jobs)).replace('@ROOT_NATIVE_JOBS@', '\n'.join(root_jobs))
            p = out / (name + '.sh')
            p.write_text(body)
            p.chmod(493)
            value['guest_mapping']['/guest/' + name + '.sh'] = {'host_path': str(p), 'sha256': digest(p)}
        content = {'source_sha': args.sha, 'artifacts': [{'guest_path': guest_destination(dst), 'sha256': row['sha256']} for dst, row in sorted(value['guest_mapping'].items())]}
        for dst, row in value['guest_mapping'].items():
            dest = art / dst.lstrip('/')
            dest.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(row['host_path'], dest)
            if digest(dest) != row['sha256']:
                raise ValueError('source changed while staging ' + dst)
        save(out / 'content.json', content)
        shutil.copy2(out / 'content.json', art / 'content.json')
        content_sha = digest(out / 'content.json')
        binds = []
        for dst in sorted(value['guest_mapping']):
            target = tree / dst.lstrip('/')
            target.parent.mkdir(parents=True, exist_ok=True)
            if target.is_symlink():
                target.unlink()
            if not target.exists():
                target.touch()
            binds += ['mount --bind ' + shlex.quote('/artifacts' + dst) + ' ' + shlex.quote(dst), 'mount -o remount,bind,ro ' + shlex.quote(dst)]
        body = (HERE / 'init.in').read_text().replace('@SOURCE_SHA@', args.sha).replace('@RUN_ID@', args.run_id).replace('@CONTENT_SHA@', content_sha).replace('@BIND_ARTIFACTS@', '\n'.join(binds)).replace('@CLI@', value['cli']).replace('@DRIVER@', value['driver'])
        (tree / 'init').write_text(body)
        (tree / 'init').chmod(493)
        archive_initramfs(tree, out / 'initramfs.cpio.gz')
        fsenv = {**env, 'LD_LIBRARY_PATH': str(args.fs_tools / 'usr/lib/x86_64-linux-gnu'), 'MKE2FS_CONFIG': str(args.fs_tools / 'etc/mke2fs.conf')}
        for name, size in [('artifacts.ext4', value['artifact_image_bytes']), ('data.btrfs', value['data_image_bytes'])]:
            fd = os.open(out / name, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 384)
            os.ftruncate(fd, size)
            os.close(fd)
        command([str(args.fs_tools / 'usr/sbin/mke2fs'), '-q', '-F', '-t', 'ext4', '-d', str(art), str(out / 'artifacts.ext4')], fsenv)
        command([str(args.fs_tools / 'usr/sbin/mkfs.btrfs'), '-f', '-d', 'single', '-m', 'dup', str(out / 'data.btrfs')], fsenv)
    st = (out / 'data.btrfs').stat()
    owner = {'schema': 'loci.btrfs-owned-data.v1', 'path': str((out / 'data.btrfs').resolve()), 'uid': os.geteuid(), 'device': st.st_dev, 'inode': st.st_ino, 'run_id': args.run_id}
    save(out / 'data-owner.json', owner)
    qemu = args.qemu
    kernel = args.kernel
    outer = {'schema': 'loci.btrfs-guest-input.v1', 'source': {'sha': args.sha}, 'sha256': {'qemu': digest(qemu), 'kernel': digest(kernel), 'initramfs': digest(out / 'initramfs.cpio.gz'), 'artifact_image': digest(out / 'artifacts.ext4')}, 'build_manifest': {'path': str(args.build_manifest), 'sha256': digest(args.build_manifest)}, 'guest_content_manifest': str(out / 'content.json'), 'guest_content_manifest_sha256': content_sha, 'data_image_owner_receipt': {'path': str(out / 'data-owner.json'), 'sha256': digest(out / 'data-owner.json')}, 'btrfs_budget': {'metadata_profile': 'DUP', 'metadata_copies': 2, 'data_bytes': 512 * 1024 * 1024, 'metadata_logical_bytes': 1024 * 1024 * 1024, 'sort_and_logs_bytes': value['result_budget_bytes']}}
    save(out / 'guest-manifest.json', outer)
    value.update(status='assembled_not_executed', environment_commands=commands, environment_build_log_sha256=digest(out / 'environment-build.log'), content_sha256=content_sha, private_environment={'qemu': str(args.qemu), 'kernel': str(args.kernel), 'base_initramfs': str(args.base_initramfs), 'fs_tools': str(args.fs_tools), 'base_initramfs_file_sha256': {str(p.relative_to(args.base_initramfs)): digest(p) for p in args.base_initramfs.rglob('*') if p.is_file() and (not p.is_symlink())}, 'base_initramfs_symlinks': {str(p.relative_to(args.base_initramfs)): os.readlink(p) for p in args.base_initramfs.rglob('*') if p.is_symlink()}}, source_helper_digests={p.name: digest(p) for p in HERE.iterdir() if p.is_file() and p.name != 'preparation.json'}, qemu=str(qemu), kernel=str(kernel))
    save(out / 'assembly.json', value)
    m.source_identity(args.source, args.sha)
    print(json.dumps({'status': value['status'], 'output': str(out), 'engine_acceptance': False}))

def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('action', choices=['source-only', 'plan'])
    p.add_argument('--output', type=pathlib.Path, required=True)
    p.add_argument('--owned-prefix', type=pathlib.Path)
    p.add_argument('--source', type=pathlib.Path)
    p.add_argument('--sha')
    p.add_argument('--build-manifest', type=pathlib.Path)
    p.add_argument('--qemu', type=pathlib.Path)
    p.add_argument('--kernel', type=pathlib.Path)
    p.add_argument('--cc', default='cc')
    p.add_argument('--rustc', default='rustc')
    p.add_argument('--base-initramfs', type=pathlib.Path)
    p.add_argument('--fs-tools', type=pathlib.Path)
    p.add_argument('--run-id')
    p.add_argument('--rust-env', type=pathlib.Path)
    p.add_argument('--assemble', action='store_true')
    a = p.parse_args()
    if a.action == 'source-only':
        source_only(a)
    else:
        if any((getattr(a, k) is None for k in ('owned_prefix','source', 'sha', 'build_manifest', 'qemu', 'kernel', 'base_initramfs', 'fs_tools', 'run_id'))):
            p.error('explicit final source/build/private environment/run token inputs required')
        plan(a)
if __name__ == '__main__':
    main()
