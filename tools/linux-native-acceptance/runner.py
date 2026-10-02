#!/usr/bin/env python3
"""Prepare exact-SHA artifacts and execute native ext4 tests in a supplied environment."""
import argparse
import datetime
import hashlib
import json
import os
from pathlib import Path
import re
import shlex
import shutil
import subprocess
import sys
import time

VERSION = 1
SPECIAL = {
    '100k': [
        ('linux_scale_100k', 'real_hundred_thousand_entries_remain_correct_after_local_updates', True, 'real100k fixture_seconds='),
        ('linux_scale_checkpoint', 'one_hundred_thousand_entry_checkpoint_saves_reopens_and_exports_full_set', True, None),
    ],
    'overflow': [
        ('linux_scale_recovery', 'actual_kernel_overflow_marker_is_observed_before_replacing_source_and_correcting', False, 'native_scale_kernel_overflow_verified'),
        ('linux_watch', 'native_kernel_overflow_bounded_fixture', True, 'PASS: real IN_Q_OVERFLOW observed'),
        ('linux_incremental', 'native_incremental_kernel_overflow', True, 'PASS: real IN_Q_OVERFLOW recovered'),
    ],
}

def utc():
    return datetime.datetime.now(datetime.timezone.utc).isoformat()

def digest(path):
    h = hashlib.sha256()
    with open(path, 'rb') as source:
        for block in iter(lambda: source.read(1024 * 1024), b''):
            h.update(block)
    return h.hexdigest()

def save(path, value):
    Path(path).write_text(json.dumps(value, indent=2, ensure_ascii=False) + '\n')

def cmd_output(argv, cwd=None):
    return subprocess.check_output(argv, cwd=cwd, text=True).strip()

def source_identity(source, sha):
    source = Path(source).resolve()
    if not re.fullmatch(r'[0-9a-f]{40}', sha):
        raise ValueError('--sha must be a full lowercase 40-hex commit ID')
    actual = cmd_output(['git', 'rev-parse', 'HEAD'], source)
    dirty = cmd_output(['git', 'status', '--porcelain', '--untracked-files=no'], source)
    if actual != sha or dirty:
        raise ValueError(f'exact clean source required: requested={sha}, actual={actual}, tracked_dirty={bool(dirty)}')
    return {'path': str(source), 'sha': actual, 'tracked_clean': True, 'tree': cmd_output(['git', 'rev-parse', 'HEAD^{tree}'], source)}

def bash_command(env_file, argv):
    if env_file is None:
        return argv
    return ['bash', '-c', 'source ' + shlex.quote(str(env_file)) + '; exec ' + shlex.join(argv)]

def stage_tools(root):
    """Owned overlay context only; do not mutate the shared probe image/rootfs."""
    root = Path(root)
    paths = set()
    for name in ('kill', 'mkfifo', 'true', 'sort'):
        tool = shutil.which(name)
        if not tool:
            raise ValueError(f'required host tool missing: {name}')
        if name == 'sort' and 'GNU coreutils' not in cmd_output([tool, '--version']):
            raise ValueError('million controller requires GNU sort with -z/-S; BusyBox sort is not accepted')
        paths.add(Path(tool).resolve())
        text = cmd_output(['ldd', tool])
        paths.update(Path(path).resolve() for path in re.findall(r'(/[^\s()]+)', text))
    items = []
    for source in sorted(paths):
        target = root / str(source).lstrip('/')
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(source, target)
        items.append({'source': str(source), 'container_path': str(source), 'snapshot': str(target), 'sha256': digest(target)})
    # Loader/library aliases must keep the ABI's conventional absolute paths.
    for alias in ('/lib64/ld-linux-x86-64.so.2', '/lib/x86_64-linux-gnu/libc.so.6', '/lib/x86_64-linux-gnu/libselinux.so.1', '/lib/x86_64-linux-gnu/libpcre2-8.so.0'):
        source = Path(alias)
        if not source.exists():
            raise ValueError(f'required host runtime alias missing: {alias}')
        target = root / alias.lstrip('/')
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(source, target)
        if not any(item['container_path'] == alias for item in items):
            items.append({'source': str(source), 'container_path': alias, 'snapshot': str(target), 'sha256': digest(target)})
    return items

def prepare(args):
    provenance = source_identity(args.source, args.sha)
    artifacts = Path(args.artifacts).resolve()
    if artifacts.exists():
        raise ValueError('artifact directory must be a fresh owned path; previous artifacts are never overwritten')
    artifacts.mkdir(parents=True)
    target = artifacts / 'target'
    manifest = {'schema': VERSION, 'prepared_utc': utc(), 'source': provenance, 'artifact_root': str(artifacts), 'target_root': str(target), 'runner_sha256': digest(__file__), 'profiles': [], 'test_artifacts': [], 'cli_artifacts': [], 'drivers': [], 'commands': [], 'production_execution': False, 'environment_file': str(Path(args.env_file).resolve()) if args.env_file else None, 'environment_file_sha256': digest(args.env_file) if args.env_file else None}
    save(artifacts / 'artifact-build-header.json', manifest)
    for profile in args.profiles:
        argv = ['cargo', 'test', '--locked', '--no-run', '--message-format=json', '--target-dir', str(target)]
        if profile == 'release':
            argv.append('--release')
        command = bash_command(args.env_file, argv)
        manifest['commands'].append(command)
        json_log = artifacts / f'build-{profile}.jsonl'
        error_log = artifacts / f'build-{profile}.stderr.log'
        with json_log.open('w') as out, error_log.open('w') as err:
            result = subprocess.run(command, cwd=provenance['path'], stdout=out, stderr=err)
        source_identity(args.source, args.sha)
        if result.returncode:
            raise ValueError(f'build failed profile={profile}; preserved {error_log}')
        for line in json_log.read_text().splitlines():
            item = json.loads(line)
            executable = item.get('executable')
            if item.get('reason') != 'compiler-artifact' or not executable or not item['profile'].get('test'):
                continue
            path = Path(executable).resolve()
            if not path.is_relative_to(target):
                raise ValueError('Cargo executable escaped the owned target root')
            manifest['test_artifacts'].append({'profile': profile, 'path': str(path), 'target': item['target'], 'cargo_profile': item['profile'], 'sha256': digest(path), 'bytes': path.stat().st_size})
        cli = target / profile / 'loci-experiment'
        if not cli.is_file():
            raise ValueError(f'compile-time child CLI missing: {cli}')
        manifest['cli_artifacts'].append({'profile': profile, 'path': str(cli), 'sha256': digest(cli), 'bytes': cli.stat().st_size})
        manifest['profiles'].append(profile)
        if args.driver_example:
            driver_argv = ['cargo', 'build', '--locked', '--example', args.driver_example, '--target-dir', str(target)]
            if profile == 'release':
                driver_argv.append('--release')
            command = bash_command(args.env_file, driver_argv)
            manifest['commands'].append(command)
            with (artifacts / f'driver-build-{profile}.log').open('w') as log:
                result = subprocess.run(command, cwd=provenance['path'], stdout=log, stderr=subprocess.STDOUT)
            source_identity(args.source, args.sha)
            if result.returncode:
                raise ValueError('requested acceptance driver build failed; no placeholder accepted')
            path = target / profile / 'examples' / args.driver_example
            manifest['drivers'].append({'profile': profile, 'path': str(path), 'sha256': digest(path), 'bytes': path.stat().st_size})
    if not manifest['test_artifacts']:
        raise ValueError('no compiled test artifacts found')
    manifest['runtime_overlay'] = stage_tools(artifacts / 'runtime-rootfs')
    (artifacts / 'Dockerfile.runtime').write_text('ARG BASE_IMAGE\nFROM ${BASE_IMAGE}\nCOPY runtime-rootfs /\n')
    # Doctests are not emitted by cargo test --no-run. Surface new code fences.
    tracked_rs = subprocess.check_output(['git', 'ls-files', '-z', '*.rs'], cwd=provenance['path']).decode().split('\0')
    manifest['doc_fence_sources'] = [relative for relative in tracked_rs if relative and re.search(r'^\s*(?:///|//!)\s*```', (Path(provenance['path']) / relative).read_text(), re.M)]
    manifest['rustc_version'] = cmd_output(bash_command(args.env_file, ['rustc', '--version', '--verbose']), provenance['path'])
    manifest['cargo_version'] = cmd_output(bash_command(args.env_file, ['cargo', '--version']), provenance['path'])
    manifest['build_log_digests'] = {path.name: digest(path) for path in artifacts.glob('*.*') if path.suffix in ('.log', '.jsonl')}
    source_identity(args.source, args.sha)
    save(artifacts / 'manifest.json', manifest)
    print(json.dumps({'status': 'artifacts_prepared_not_executed', 'manifest': str(artifacts / 'manifest.json'), 'profiles': manifest['profiles']}))

def verify_manifest(manifest):
    source_identity(manifest['source']['path'], manifest['source']['sha'])
    for item in manifest['test_artifacts'] + manifest['cli_artifacts'] + manifest['drivers']:
        if digest(item['path']) != item['sha256']:
            raise ValueError(f'artifact digest mismatch: {item["path"]}')
    for item in manifest['runtime_overlay']:
        if digest(item['snapshot']) != item['sha256']:
            raise ValueError(f'runtime overlay digest mismatch: {item["snapshot"]}')

def mount(source, destination, readonly=True):
    if any(char in str(source) + destination for char in ',\n\r'):
        raise ValueError('Docker --mount paths cannot contain comma/newline')
    return ['--mount', f'type=bind,src={source},dst={destination}' + (',readonly' if readonly else '')]

def jobs_for(manifest, phases, timeout):
    jobs = []
    for profile in manifest['profiles']:
        artifacts = [item for item in manifest['test_artifacts'] if item['profile'] == profile]
        if 'suites' in phases:
            for index, artifact in enumerate(artifacts):
                target = re.sub(r'[^a-zA-Z0-9_-]', '_', artifact['target']['name'])
                jobs.append({'id': f'{profile}.suite.{index:03d}.{target}', 'profile': profile, 'phase': 'suites', 'argv': [artifact['path'], '--test-threads=1', '--nocapture'], 'timeout': timeout, 'required_marker': None, 'required_one_test': False})
        for phase in ('100k', 'overflow'):
            if phase not in phases:
                continue
            for target, test, ignored, marker in SPECIAL[phase]:
                matches = [item for item in artifacts if item['target']['name'] == target and 'test' in item['target']['kind']]
                if len(matches) != 1:
                    raise ValueError(f'required artifact not unique: {profile}/{target}')
                argv = [matches[0]['path'], '--exact', test, '--test-threads=1', '--nocapture']
                if ignored:
                    argv.append('--ignored')
                jobs.append({'id': f'{profile}.{phase}.{target}', 'profile': profile, 'phase': phase, 'test': test, 'argv': argv, 'timeout': timeout, 'required_marker': marker, 'required_one_test': True})
    return jobs

def native_script(jobs, results, run_id, million=None, launcher='/usr/bin/loci-run-ext4'):
    q = shlex.quote
    lines = ['#!/usr/bin/bash', 'set -u', f'RESULTS={q(str(results))}', f'RUN_ID={q(run_id)}', '[[ $(id -u) == 1000 && $(id -g) == 1000 ]] || exit 90', '[[ $(stat -f -c %T /mnt/loci-native) == ext2/ext3 ]] || exit 91', 'findmnt -T /mnt/loci-native -o TARGET,SOURCE,FSTYPE,OPTIONS > "$RESULTS/native-mount.txt"', '[[ $(findmnt -T /mnt/loci-native -n -o FSTYPE) == ext4 ]] || exit 91', f'sha256sum {q(launcher)} > "$RESULTS/native-launcher-sha256.txt"', 'id > "$RESULTS/native-identity.txt"', 'uname -a > "$RESULTS/native-kernel.txt"', 'cat /proc/cpuinfo > "$RESULTS/native-cpuinfo.txt"', 'cat /proc/meminfo > "$RESULTS/native-meminfo.txt"', 'cat /proc/self/cgroup > "$RESULTS/native-cgroup.txt"', 'for loci_file in /sys/fs/cgroup/memory.max /sys/fs/cgroup/cpu.max /proc/sys/fs/inotify/max_user_watches /proc/sys/fs/inotify/max_user_instances /proc/sys/fs/inotify/max_queued_events; do printf "%s=" "$loci_file"; cat "$loci_file"; done > "$RESULTS/native-limits.txt"', 'df -Pk /mnt/loci-native > "$RESULTS/native-disk.txt"', 'df -Pi /mnt/loci-native > "$RESULTS/native-inodes.txt"', 'mkdir -p "/mnt/loci-native/$RUN_ID"', ': > "$RESULTS/commands.tsv"']
    for job in jobs:
        work = f'/mnt/loci-native/{run_id}/{job["id"]}'
        log = str(results / (job['id'] + '.log'))
        argv = ['timeout', '--signal=TERM', '--kill-after=5s', f'{job["timeout"]}s'] + job['argv']
        lines += [f'mkdir -p {q(work)}', f'cd {q(work)}', 'LOCI_START=$SECONDS', f'{shlex.join(argv)} > {q(log)} 2>&1', 'LOCI_STATUS=$?', f'printf "%s\\t%s\\t%s\\n" {q(job["id"])} "$LOCI_STATUS" "$((SECONDS-LOCI_START))" >> "$RESULTS/commands.tsv"', f'printf "completed %s exit=%s\\n" {q(job["id"])} "$LOCI_STATUS"']
    if million:
        # Fresh launcher owns this root. Copy actual files onto ext4, not a symlink to overlayfs.
        base = f'/mnt/loci-native/{run_id}/million'
        lines += ['mapfile -t LOCI_DISK_ROWS < <(df -Pk /mnt/loci-native)', 'read -r LOCI_FS LOCI_TOTAL LOCI_USED LOCI_FREE LOCI_REST <<<"${LOCI_DISK_ROWS[1]}"', '[[ $LOCI_TOTAL =~ ^[0-9]+$ && $LOCI_FREE =~ ^[0-9]+$ ]] || exit 93', '(( LOCI_FREE >= 1572864 && LOCI_FREE * 100 >= LOCI_TOTAL * 15 )) || exit 93', 'mapfile -t LOCI_INODE_ROWS < <(df -Pi /mnt/loci-native)', 'read -r LOCI_FS LOCI_TOTAL LOCI_USED LOCI_FREE LOCI_REST <<<"${LOCI_INODE_ROWS[1]}"', '[[ $LOCI_TOTAL =~ ^[0-9]+$ && $LOCI_FREE =~ ^[0-9]+$ ]] || exit 93', '(( LOCI_FREE >= 1150000 && LOCI_FREE * 100 >= LOCI_TOTAL * 15 )) || exit 93', f'mkdir -p {q(base)}', f'cp -a /input {q(base + "/data")}', 'LOCI_COPY=$?', '[[ $LOCI_COPY == 0 ]] || exit 92', f'mkdir -p {q(str(results / "million"))}', 'LOCI_START=$SECONDS', f'{shlex.join(million["argv"])} > {q(str(results / "million.log"))} 2>&1', 'LOCI_STATUS=$?', 'printf "million\\t%s\\t%s\\n" "$LOCI_STATUS" "$((SECONDS-LOCI_START))" >> "$RESULTS/commands.tsv"', f'if [[ -f {q(base + "/checkpoint.loci")} ]]; then cp {q(base + "/checkpoint.loci")} {q(str(results / "million" / "checkpoint.loci"))}; fi']
    return '\n'.join(lines) + '\n'

SKIP = re.compile(r'\b(SKIP|UNVERIFIED):\s*(.*)')
TEST = re.compile(r'^test (\S+) \.\.\.\s*(.*)')
SUMMARY = re.compile(r'test result: (?:ok|FAILED)\. (\d+) passed; (\d+) failed; (\d+) ignored;')

def classify_log(text, returncode, job):
    current = None
    skips, ignored = [], []
    for line in text.splitlines():
        named = TEST.match(line)
        if named:
            current = named[1]
            if named[2].startswith('ignored'):
                ignored.append({'test': current, 'reason': named[2]})
        marker = SKIP.search(line)
        if marker:
            skips.append({'test': current or job.get('test') or 'unknown_test_requires_manual_attribution', 'kind': marker[1], 'reason': marker[2], 'raw_line': line})
    summaries = SUMMARY.findall(text)
    counts = dict(zip(('passed', 'failed', 'ignored'), map(int, summaries[-1]))) if summaries else None
    failures = []
    if returncode != 0:
        failures.append(f'exit_status={returncode}')
    if not counts:
        failures.append('Rust test completion summary missing')
    elif counts['failed']:
        failures.append('Rust failures reported')
    if job.get('required_one_test') and counts and (counts['passed'] != 1 or counts['ignored']):
        failures.append('required exact test did not run exactly once')
    marker = job.get('required_marker')
    if marker and marker not in text and not skips:
        failures.append(f'positive native proof marker missing: {marker}')
    unidentified = any(item['test'] == 'unknown_test_requires_manual_attribution' for item in skips)
    skipped_tests = {item['test'] for item in skips}
    credited = max(0, counts['passed'] - len(skipped_tests)) if counts and not unidentified else None
    return {'status': 'failed' if failures else ('unverified' if skips else 'passed'), 'libtest_counts_not_acceptance_counts': counts, 'passed_test_count_without_environment_skips': credited, 'unverified_test_count': len(skipped_tests), 'environment_unverified': skips, 'ignored_not_passed': ignored, 'failure_reasons': failures}

def run(args):
    manifest_path = Path(args.manifest).resolve()
    manifest = json.loads(manifest_path.read_text())
    verify_manifest(manifest)
    if manifest['profiles'] != ['debug', 'release']:
        raise ValueError('final native run requires both debug and release artifacts')
    results = Path(args.results).resolve()
    if results.exists():
        raise ValueError('results directory must be fresh; raw results are never overwritten')
    results.mkdir(parents=True)
    if os.geteuid() not in (0, 1000):
        raise ValueError('host result ownership requires UID1000 or root')
    if os.geteuid() == 0:
        os.chown(results, 1000, 1000)
    os.chmod(results, 0o755)
    phases = args.phases.split(',')
    if set(phases) - {'suites', '100k', 'overflow', 'million'}:
        raise ValueError('unknown phase')
    jobs = jobs_for(manifest, phases, args.test_timeout)
    run_id = 'ext4-' + manifest['source']['sha'][:12] + '-' + str(time.time_ns())
    image_id = cmd_output(['docker', 'image', 'inspect', '--format', '{{.Id}}', args.image])
    if image_id != args.image_id:
        raise ValueError(f'probe image identity mismatch: expected={args.image_id}, actual={image_id}')
    million = None
    if 'million' in phases:
        if not args.fixture_data or not args.queries:
            raise ValueError('million phase requires exact real fixture data and queries paths')
        driver = [item for item in manifest['drivers'] if item['profile'] == 'release']
        if len(driver) != 1:
            raise ValueError('required real release driver missing; no placeholder can be executed')
        base = f'/mnt/loci-native/{run_id}/million'
        million = {'argv': ['timeout', '--signal=TERM', '--kill-after=5s', f'{args.million_timeout}s', driver[0]['path'], '--controller', '--root', base + '/data', '--database', base + '/checkpoint.loci', '--output', str(results / 'million'), '--sha', manifest['source']['sha'], '--queries', str(Path(args.queries).resolve()), '--phase', 'all', '--repetitions', '200', '--idle-seconds', '600']}
    script = results / 'native-plan.sh'
    script.write_text(native_script(jobs, results, run_id, million, args.container_launcher))
    command = ['docker', 'run', '--rm', '--name', run_id, '--network', 'none', '--cap-add', 'SYS_ADMIN', '--device-cgroup-rule', 'b 7:* rwm', '--device-cgroup-rule', 'c 10:237 rwm']
    command += mount(manifest['source']['path'], manifest['source']['path'])
    command += mount(manifest['artifact_root'], manifest['artifact_root'])
    command += mount(str(results), str(results), readonly=False)
    for item in manifest['runtime_overlay']:
        command += mount(item['snapshot'], item['container_path'])
    if million:
        command += mount(str(Path(args.fixture_data).resolve()), '/input')
        command += mount(str(Path(args.queries).resolve()), str(Path(args.queries).resolve()))
    command += [image_id, args.container_launcher, '/usr/bin/bash', str(script)]
    header = {'schema': VERSION, 'run_id': run_id, 'sha': manifest['source']['sha'], 'started_utc': utc(), 'image_id': image_id, 'container_launcher': args.container_launcher, 'uid': 1000, 'filesystem_requested': 'ext4', 'reference_hardware_verified': False, 'artifact_manifest': str(manifest_path), 'artifact_manifest_sha256': digest(manifest_path), 'artifact_digests': manifest['test_artifacts'] + manifest['cli_artifacts'] + manifest['drivers'], 'command': command, 'jobs': jobs, 'selected_phases': phases, 'million_driver': million, 'full_issue15_acceptance_claimed': False}
    save(results / 'run-header.json', header)
    if args.plan_only:
        print(json.dumps({'status': 'planned_not_executed', 'results': str(results)}))
        return
    began = time.monotonic()
    timed_out = False
    with (results / 'container.log').open('w') as log:
        try:
            result = subprocess.run(command, stdout=log, stderr=subprocess.STDOUT, timeout=args.container_timeout)
            container_code = result.returncode
        except subprocess.TimeoutExpired:
            timed_out = True
            subprocess.run(['docker', 'rm', '-f', run_id], stdout=log, stderr=subprocess.STDOUT)
            container_code = 124
    records = {}
    status_file = results / 'commands.tsv'
    if status_file.exists():
        for line in status_file.read_text().splitlines():
            name, code, seconds = line.split('\t')
            records[name] = (int(code), int(seconds))
    findings = []
    for job in jobs:
        log_path = results / (job['id'] + '.log')
        code, elapsed = records.get(job['id'], (-1, 0))
        finding = classify_log(log_path.read_text(errors='replace') if log_path.exists() else '', code, job)
        finding.update({'id': job['id'], 'profile': job['profile'], 'phase': job['phase'], 'exact_test': job.get('test'), 'exit_status': code, 'elapsed_seconds': elapsed, 'log': str(log_path), 'log_sha256': digest(log_path) if log_path.exists() else None})
        findings.append(finding)
    environment = [dict(item, job=finding['id']) for finding in findings for item in finding['environment_unverified']]
    provenance_error = None
    try:
        verify_manifest(manifest)
    except (ValueError, OSError, subprocess.CalledProcessError) as error:
        provenance_error = str(error)
    status = 'failed' if container_code or provenance_error or any(item['status'] == 'failed' for item in findings) else ('unverified' if environment or manifest['doc_fence_sources'] else 'passed')
    # Controller logs/manifests contain the 1M requirement outcomes, not test-summary totals.
    million_status = None
    if million:
        code, elapsed = records.get('million', (-1, 0))
        million_status = {'execution_exit_status': code, 'elapsed_seconds': elapsed, 'output': str(results / 'million'), 'acceptance_status': 'requires_driver_manifest_assessment_even_when_exit_zero'}
        if code:
            status = 'failed'
    summary = {'schema': VERSION, 'run_id': run_id, 'sha': manifest['source']['sha'], 'completed_utc': utc(), 'elapsed_wall_seconds': time.monotonic() - began, 'execution_status': status, 'post_run_provenance_error': provenance_error, 'container_exit_status': container_code, 'container_timeout': timed_out, 'jobs': findings, 'environment_unverified': environment, 'doctest_code_fences_requiring_separate_execution': manifest['doc_fence_sources'], 'million': million_status, 'full_issue15_acceptance_claimed': False, 'performance_reference_hardware_verified': False, 'durability_scope': 'process/crash tests only; no physical power-loss claim', 'raw_result_digests': {path.name: digest(path) for path in results.iterdir() if path.is_file()}}
    save(results / 'summary.json', summary)
    print(json.dumps({'status': status, 'summary': str(results / 'summary.json'), 'environment_unverified_tests': len(environment)}))
    if status != 'passed':
        raise SystemExit(1 if status == 'failed' else 4)

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest='action', required=True)
    prep = sub.add_parser('prepare', help='explicitly compile clean exact-SHA artifacts; never executes tests')
    prep.add_argument('--source', type=Path, required=True)
    prep.add_argument('--sha', required=True)
    prep.add_argument('--artifacts', type=Path, required=True)
    prep.add_argument('--rust-env', '--env-file', dest='env_file', type=Path, help='optional shell environment file; otherwise use configured PATH')
    prep.add_argument('--profiles', nargs='+', choices=('debug', 'release'), default=['debug', 'release'])
    prep.add_argument('--driver-example')
    execute = sub.add_parser('run', help='run prepared exact artifacts on UID1000 ext4; --plan-only never starts a container')
    execute.add_argument('--manifest', type=Path, required=True)
    execute.add_argument('--results', type=Path, required=True)
    execute.add_argument('--phases', default='suites,100k,overflow')
    execute.add_argument('--image', required=True, help='image supplied by environment preparation; not bundled in this repository')
    execute.add_argument('--image-id', required=True, help='expected docker image ID, e.g. sha256:...')
    execute.add_argument('--container-launcher', default='/usr/bin/loci-run-ext4', help='trusted image launcher supplied by environment preparation')
    execute.add_argument('--test-timeout', type=int, default=600)
    execute.add_argument('--container-timeout', type=int, default=7200)
    execute.add_argument('--fixture-data', type=Path)
    execute.add_argument('--queries', type=Path, help='UTF-8 newline-text query suite, one literal query per line; not JSON')
    execute.add_argument('--million-timeout', type=int, default=3600)
    execute.add_argument('--plan-only', action='store_true')
    classify = sub.add_parser('classify-log', help='assess one retained Rust test log without building or running workloads')
    classify.add_argument('--log', type=Path, required=True)
    classify.add_argument('--exit-status', type=int, required=True)
    classify.add_argument('--required-marker')
    classify.add_argument('--required-one-test', action='store_true')
    classify.add_argument('--test-name')
    args = parser.parse_args()
    if args.action == 'prepare':
        prepare(args)
    elif args.action == 'run':
        run(args)
    else:
        result = classify_log(args.log.read_text(errors='replace'), args.exit_status, {'required_marker': args.required_marker, 'required_one_test': args.required_one_test, 'test': args.test_name})
        print(json.dumps(result, indent=2))
        if result['status'] != 'passed':
            raise SystemExit(1 if result['status'] == 'failed' else 4)

if __name__ == '__main__':
    try:
        main()
    except (ValueError, OSError, subprocess.CalledProcessError) as error:
        print(f'runner: {error}', file=sys.stderr)
        raise SystemExit(2)
