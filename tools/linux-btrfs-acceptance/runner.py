#!/usr/bin/env python3
"""Check supplied immutable artifacts and retain a bounded, networkless Btrfs guest run."""
import argparse
import datetime
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import signal
import subprocess
import sys
import time

SHA = re.compile(r'[0-9a-f]{40}')
DIGEST = re.compile(r'[0-9a-f]{64}')
SCHEMA = 'loci.btrfs-guest-input.v1'


def linux_tools():
    path = Path(__file__).resolve().parents[1] / 'linux-native-acceptance' / 'runner.py'
    spec = importlib.util.spec_from_file_location('loci_native_artifacts', path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def file_digest(path):
    checksum = hashlib.sha256()
    with Path(path).open('rb') as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b''):
            checksum.update(chunk)
    return checksum.hexdigest()


def save(path, value):
    Path(path).write_text(json.dumps(value, indent=2) + '\n')


def assess(body, exit_status, sha, content_sha, run_id, timed_out=False):
    lines = set(body.decode('utf-8', errors='replace').splitlines())
    required = {
        f'LOCI_BTRFS_SOURCE sha={sha} content_sha256={content_sha} run_id={run_id}',
        'LOCI_BTRFS_ARTIFACT_READONLY verified=1',
        f'LOCI_BTRFS_WORKLOAD_PASS run_id={run_id}',
        f'LOCI_BTRFS_COMPLETE run_id={run_id}',
    }
    native_matches = [re.fullmatch(
        r'NATIVE_CAPABILITY uid=1000 gid=1000 statfs=9123683e mount_id=(\d+) inode=(\d+) inotify_bytes=(\d+) create=1 close_write=1', line
    ) for line in lines]
    native = any(match and all(int(value) > 0 for value in match.groups()) for match in native_matches)
    forbidden = [line for line in lines if re.search(r'(?:\bSKIP:|\bUNVERIFIED:|\bGUEST_CAPABILITY_FAIL\b|\bLOCI_BTRFS_FAIL\b)', line)]
    missing = sorted(required - lines)
    stats = [re.fullmatch(r'NATIVE_STATVFS files=(\d+) ffree=(\d+) favail=(\d+) blocks=(\d+) bfree=(\d+) bavail=(\d+) frsize=(\d+)', line) for line in lines]
    stats = [match for match in stats if match]
    inode_capacity = {'status': 'unavailable', 'reason': 'statvfs evidence missing', 'metadata_capacity_verified': False}
    if len(stats) == 1:
        measured = dict(zip(('files', 'ffree', 'favail', 'blocks', 'bfree', 'bavail', 'frsize'), map(int, stats[0].groups())))
        inode_capacity = {'status': 'unavailable' if measured['files'] == 0 else 'reported',
                          'reason': 'Btrfs dynamic inode counter; f_files=0' if measured['files'] == 0 else None,
                          'measured': measured, 'metadata_capacity_verified': False}
    complete = not timed_out and exit_status == 0 and not missing and native and len(stats) == 1 and not forbidden
    return {'status': 'passed' if complete else 'failed', 'missing_markers': missing,
            'native_uid_btrfs_inotify_proof': native, 'inode_capacity': inode_capacity, 'failure_or_unverified_lines': forbidden,
            'timed_out': timed_out, 'guest_workload_evidence_only': True,
            'engine_acceptance': False, 'issue16_resolved': False,
            'hardware_performance_acceptance': False}


def owned_file(value, prefix):
    supplied = Path(value).absolute()
    if any(part.is_symlink() for part in (supplied, *supplied.parents)):
        raise ValueError(f'symlink input/ancestor is unsupported: {value}')
    path = supplied.resolve(strict=True)
    path.relative_to(prefix)
    if not path.is_file():
        raise ValueError(f'regular task-owned file required: {value}')
    if path.stat().st_uid != os.geteuid():
        raise ValueError(f'file is not owned by this user: {path}')
    return path


def terminate_owned(process, pidfd):
    evidence = {'root_exit_confirmed': False, 'descendant_release_confirmed': False,
                'target': 'held pidfd for direct child only', 'error': None}
    try:
        if process.poll() is None:
            signal.pidfd_send_signal(pidfd, signal.SIGTERM)
            try:
                process.wait(timeout=3)
            except subprocess.TimeoutExpired:
                signal.pidfd_send_signal(pidfd, signal.SIGKILL)
                process.wait(timeout=3)
        evidence['root_exit_confirmed'] = process.poll() is not None
    except (OSError, subprocess.TimeoutExpired) as exc:
        evidence['error'] = str(exc)
    return evidence


def plan(args):
    helpers = linux_tools()
    prefix = args.owned_prefix.resolve(strict=True)
    if not prefix.is_dir() or prefix.stat().st_uid != os.geteuid() or prefix.stat().st_mode & 0o022:
        raise ValueError('owned prefix must be an existing user-owned directory without group/other write access')
    files = {key: owned_file(getattr(args, key), prefix)
             for key in ('qemu', 'kernel', 'initramfs', 'artifact_image', 'data_image')}
    if files['data_image'].stat().st_nlink != 1:
        raise ValueError('writable data image cannot have hard-link aliases')
    if files['data_image'] in [value for key, value in files.items() if key != 'data_image']:
        raise ValueError('writable data disk must differ from all immutable inputs')
    if not os.access(files['qemu'], os.X_OK):
        raise ValueError('explicit QEMU executable is not executable')
    manifest_path = args.artifact_manifest.resolve(strict=True)
    manifest = json.loads(manifest_path.read_text())
    if manifest.get('schema') != SCHEMA or manifest.get('source', {}).get('sha') != args.sha:
        raise ValueError('final guest manifest schema/source SHA mismatch; old capability receipts are insufficient')
    source = helpers.source_identity(args.source, args.sha)
    owner = manifest.get('data_image_owner_receipt', {})
    owner_path = owned_file(owner.get('path', ''), prefix)
    if owner.get('sha256') != file_digest(owner_path):
        raise ValueError('data-image ownership receipt digest mismatch')
    ownership = json.loads(owner_path.read_text())
    data_stat = files['data_image'].stat()
    if (ownership.get('schema') != 'loci.btrfs-owned-data.v1' or ownership.get('path') != str(files['data_image'])
            or ownership.get('uid') != os.geteuid() or ownership.get('device') != data_stat.st_dev
            or ownership.get('inode') != data_stat.st_ino or ownership.get('run_id') != args.run_id):
        raise ValueError('existing writable data image lacks its exact declared ownership/run receipt')
    for key in ('qemu', 'kernel', 'initramfs', 'artifact_image'):
        if manifest.get('sha256', {}).get(key) != file_digest(files[key]):
            raise ValueError(f'{key} immutable input digest mismatch')
    content_sha = manifest.get('guest_content_manifest_sha256', '')
    if not DIGEST.fullmatch(content_sha):
        raise ValueError('guest content manifest SHA256 required')
    build = manifest.get('build_manifest', {})
    build_path = Path(build.get('path', '')).resolve(strict=True)
    if build.get('sha256') != file_digest(build_path):
        raise ValueError('shared exact-SHA build manifest digest mismatch')
    build_manifest = json.loads(build_path.read_text())
    if build_manifest.get('source', {}).get('sha') != args.sha:
        raise ValueError('build manifest source SHA mismatch')
    if not build_manifest.get('commands') or not build_manifest.get('rustc_version') or not build_manifest.get('profiles'):
        raise ValueError('compile receipt with compiler/profile/commands required; a claimed source SHA is insufficient')
    if not build_manifest.get('cli_artifacts') or not build_manifest.get('test_artifacts'):
        raise ValueError('compiled CLI and test artifacts required')
    helpers.verify_manifest(build_manifest)
    log_digests = build_manifest.get('build_log_digests', {})
    if not log_digests:
        raise ValueError('retained compile-log digests required')
    for name, expected_digest in log_digests.items():
        if Path(name).name != name or file_digest(build_path.parent / name) != expected_digest:
            raise ValueError('retained compile-log receipt mismatch')
    content_path = owned_file(manifest.get('guest_content_manifest', ''), prefix)
    if file_digest(content_path) != content_sha:
        raise ValueError('guest content manifest digest mismatch')
    content = json.loads(content_path.read_text())
    if content.get('source_sha') != args.sha:
        raise ValueError('guest content manifest source SHA mismatch')
    entries = content.get('artifacts', [])
    copied = {item['guest_path']: item['sha256'] for item in entries}
    if len(copied) != len(entries) or any(not Path(key).is_absolute() for key in copied):
        raise ValueError('guest artifact destinations must be unique absolute paths')
    expected = [(item['path'], item['sha256']) for item in build_manifest['test_artifacts'] + build_manifest['cli_artifacts'] + build_manifest.get('drivers', [])]
    expected += [(item['container_path'], item['sha256']) for item in build_manifest['runtime_overlay']]
    if '/usr/bin/sort' not in dict(expected):
        raise ValueError('GNU sort canonical runtime snapshot missing; prepare with the current shared Linux runner')
    for destination, digest in expected:
        if copied.get(destination) != digest:
            raise ValueError(f'guest content does not preserve exact compiled artifact/runtime path and digest: {destination}')
    budget = manifest.get('btrfs_budget', {})
    if budget.get('metadata_profile') != 'DUP' or budget.get('metadata_copies') != 2:
        raise ValueError('explicit Btrfs DUP metadata budget with two copies required')
    amounts = [budget.get(key) for key in ('data_bytes', 'metadata_logical_bytes', 'sort_and_logs_bytes')]
    if any(type(value) is not int or value <= 0 for value in amounts):
        raise ValueError('positive data, logical metadata and sort/log budget bytes required')
    physical = amounts[0] + 2 * amounts[1] + amounts[2]
    if physical * 100 > files['data_image'].stat().st_size * 85:
        raise ValueError('guest logical image cannot retain 15% reserve with data plus duplicated metadata budget')
    st = os.statvfs(prefix)
    available, total = st.f_bavail * st.f_frsize, st.f_blocks * st.f_frsize
    if args.host_write_budget_bytes < files['data_image'].stat().st_size + args.max_log_bytes:
        raise ValueError('host write budget must reserve full data-image growth plus serial log budget')
    if (available - args.host_write_budget_bytes) * 100 < total * 15:
        raise ValueError('host backing filesystem cannot retain 15% reserve after planned writes')
    qemu_args = [str(files['qemu']), '-machine', 'q35,accel=tcg', '-cpu', 'max',
                 '-smp', str(args.vcpus), '-m', str(args.memory_mib), '-nodefaults',
                 '-display', 'none', '-serial', 'stdio', '-monitor', 'none', '-nic', 'none',
                 '-kernel', str(files['kernel']), '-initrd', str(files['initramfs']),
                 '-append', f'console=ttyS0 rdinit=/init panic=-1 loci_sha={args.sha} loci_run_id={args.run_id}',
                 '-drive', f'file={files["artifact_image"]},format=raw,if=virtio,readonly=on',
                 '-drive', f'file={files["data_image"]},format=raw,if=virtio', '-no-reboot']
    environment_inputs = {}
    for key, flag in (('qemu_data_dir', '-L'), ('bios', '-bios'), ('qemu_library_dir', None)):
        value = getattr(args, key)
        if value:
            path = value.resolve(strict=True)
            path.relative_to(prefix)
            if ',' in str(path):
                raise ValueError('comma in QEMU input path is unsupported')
            if key == 'bios':
                files['bios'] = owned_file(value, prefix)
            elif not path.is_dir():
                raise ValueError(f'{key} must be a supplied directory')
            environment_inputs[key] = str(path)
            if flag:
                qemu_args += [flag, str(path)]
    if any(',' in str(path) for path in files.values()):
        raise ValueError('comma in QEMU input path is unsupported')
    return {'schema': SCHEMA, 'source': source, 'run_id': args.run_id,
            'artifact_manifest': str(manifest_path), 'artifact_manifest_sha256': file_digest(manifest_path),
            'build_manifest': build, 'guest_content_manifest_sha256': content_sha,
            'immutable_inputs': {key: {'path': str(files[key]), 'sha256': file_digest(files[key])}
                                 for key in files if key != 'data_image'},
            'environment_inputs': environment_inputs,
            'data_image': str(files['data_image']), 'data_image_logical_bytes': files['data_image'].stat().st_size,
            'btrfs_budget': budget, 'budgeted_physical_bytes': physical,
            'host_budget': {'available_bytes': available, 'total_bytes': total,
                            'planned_write_bytes': args.host_write_budget_bytes, 'reserve_percent': 15},
            'command': qemu_args, 'runtime_network': 'none', 'accelerator': 'TCG',
            'vcpus': args.vcpus, 'memory_mib': args.memory_mib,
            'engine_acceptance': False, 'issue16_resolved': False}


def run(args):
    if not hasattr(os, 'pidfd_open') or not hasattr(signal, 'pidfd_send_signal'):
        raise ValueError('Linux pidfd support required for held-process timeout termination')
    header = plan(args)
    results = args.results.resolve()
    results.relative_to(args.owned_prefix.resolve())
    results.mkdir()  # Fresh evidence only; never overwrite a previous run.
    save(results / 'plan.json', header)
    if args.plan_only:
        print(json.dumps({'status': 'planned_not_executed', 'results': str(results)}))
        return
    began = time.monotonic()
    started_utc = datetime.datetime.now(datetime.timezone.utc).isoformat()
    log = results / 'serial.log'
    process = None
    pidfd = None
    timed_out = False
    failure = None
    termination = None
    exit_status = None
    try:
        with log.open('xb') as output:
            environment = os.environ.copy()
            if args.qemu_library_dir:
                library_dir = args.qemu_library_dir.resolve(strict=True)
                library_dir.relative_to(args.owned_prefix.resolve())
                environment['LD_LIBRARY_PATH'] = str(library_dir)
            process = subprocess.Popen(header['command'], stdin=subprocess.DEVNULL,
                                       stdout=output, stderr=subprocess.STDOUT, start_new_session=True, env=environment)
            try:
                pidfd = os.pidfd_open(process.pid)
            except ProcessLookupError:
                # The direct child remains waitable; an already finished child
                # needs no signal or PID-based reacquisition.
                exit_status = process.wait(timeout=1)
            while exit_status is None:
                try:
                    exit_status = process.wait(timeout=min(0.1, max(0.001, args.timeout - (time.monotonic() - began))))
                except subprocess.TimeoutExpired:
                    if time.monotonic() - began >= args.timeout or log.stat().st_size > args.max_log_bytes:
                        timed_out = time.monotonic() - began >= args.timeout
                        failure = 'guest timeout; incomplete workload' if timed_out else 'serial log budget exceeded'
                        termination = terminate_owned(process, pidfd)
                        exit_status = process.returncode
                        break
    except Exception as exc:
        failure = str(exc)
        if process and process.poll() is None:
            if pidfd is not None:
                termination = terminate_owned(process, pidfd)
                exit_status = process.returncode
            else:
                termination = {'root_exit_confirmed': False, 'descendant_release_confirmed': False,
                               'error': 'pidfd acquisition failed; process release is unconfirmed'}
    finally:
        if pidfd is not None:
            os.close(pidfd)
    body = b''
    if log.exists():
        with log.open('rb') as captured:
            body = captured.read(args.max_log_bytes + 1)
    if len(body) > args.max_log_bytes:
        failure = 'serial log budget exceeded; raw evidence retained, no passing truncation'
    finding = assess(body, exit_status, args.sha, header['guest_content_manifest_sha256'], args.run_id, timed_out)
    if failure:
        finding['status'] = 'failed'
    # A second immutable-input check catches host-side replacement during run.
    helpers = linux_tools()
    changed = []
    for key, item in header['immutable_inputs'].items():
        try:
            if file_digest(item['path']) != item['sha256']:
                changed.append(key)
        except OSError:
            changed.append(key)
    if changed:
        finding['status'] = 'failed'
        failure = f'immutable inputs changed during run: {changed}'
    try:
        helpers.source_identity(args.source, args.sha)
    except (OSError, ValueError, subprocess.CalledProcessError) as exc:
        finding['status'] = 'failed'
        failure = f'source identity changed: {exc}'
    report = {**finding, 'source_sha': args.sha, 'run_id': args.run_id,
              'started_utc': started_utc,
              'elapsed_seconds': time.monotonic() - began, 'timeout_seconds': args.timeout,
              'exit_status': exit_status, 'process_id': process.pid if process else None, 'error': failure, 'termination': termination,
              'raw_log': str(log), 'raw_log_sha256': file_digest(log) if log.exists() and process is not None and process.poll() is not None else None,
              'log_complete': process is not None and process.poll() is not None and not failure and finding['status'] == 'passed',
              'log_completion_basis': 'explicit guest completion and direct-child exit; descendant release unconfirmed',
              'btrfs_inode_capacity': 'unavailable when statfs f_files=0; no ext4 inode estimate substituted'}
    save(results / 'summary.json', report)
    print(json.dumps({'status': report['status'], 'summary': str(results / 'summary.json')}))
    if report['status'] != 'passed':
        raise SystemExit(1)


def positive(value):
    result = int(value)
    if result <= 0:
        raise argparse.ArgumentTypeError('positive value required')
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest='action', required=True)
    check = sub.add_parser('classify-log', help='classify retained guest evidence; never boots a VM')
    check.add_argument('--log', type=Path, required=True)
    check.add_argument('--exit-status', type=int, required=True)
    check.add_argument('--sha', required=True)
    check.add_argument('--content-sha256', required=True)
    check.add_argument('--run-id', required=True)
    check.add_argument('--timed-out', action='store_true')
    launch = sub.add_parser('run', help='use an externally prepared guest; --plan-only does not execute QEMU')
    for key in ('owned-prefix', 'source', 'qemu', 'kernel', 'initramfs', 'artifact-image', 'data-image', 'artifact-manifest', 'results'):
        launch.add_argument('--' + key, type=Path, required=True)
    launch.add_argument('--sha', required=True)
    launch.add_argument('--run-id', required=True)
    launch.add_argument('--qemu-data-dir', type=Path)
    launch.add_argument('--bios', type=Path)
    launch.add_argument('--qemu-library-dir', type=Path)
    launch.add_argument('--timeout', type=positive, default=1800)
    launch.add_argument('--max-log-bytes', type=positive, default=256 * 1024 * 1024)
    launch.add_argument('--host-write-budget-bytes', type=positive, required=True)
    launch.add_argument('--vcpus', type=positive, default=2)
    launch.add_argument('--memory-mib', type=positive, default=2048)
    launch.add_argument('--plan-only', action='store_true')
    args = parser.parse_args()
    if not SHA.fullmatch(args.sha) or not re.fullmatch(r'[A-Za-z0-9_-]{1,80}', args.run_id):
        parser.error('full lowercase 40-hex SHA and simple unique run-id required')
    if args.action == 'classify-log':
        if not DIGEST.fullmatch(args.content_sha256):
            parser.error('full lowercase content SHA256 required')
        result = assess(args.log.read_bytes(), args.exit_status, args.sha, args.content_sha256, args.run_id, args.timed_out)
        print(json.dumps(result))
        if result['status'] != 'passed':
            raise SystemExit(1)
    else:
        run(args)


if __name__ == '__main__':
    try:
        main()
    except (OSError, ValueError, KeyError, subprocess.CalledProcessError) as exc:
        print(json.dumps({'status': 'failed', 'error': str(exc), 'engine_acceptance': False}), file=sys.stderr)
        raise SystemExit(1)
