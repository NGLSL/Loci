"""Tiny public-CLI protocol/safety checks. No QEMU, Rust or Btrfs execution."""
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

RUNNER = Path(__file__).with_name('runner.py')
SHA = 'a' * 40
CONTENT = 'b' * 64
RUN_ID = 'synthetic-test'


def digest(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def passing_lines():
    return '\n'.join([
        f'LOCI_BTRFS_SOURCE sha={SHA} content_sha256={CONTENT} run_id={RUN_ID}',
        'LOCI_BTRFS_ARTIFACT_READONLY verified=1',
        'NATIVE_CAPABILITY uid=1000 gid=1000 statfs=9123683e mount_id=32 inode=7 inotify_bytes=64 create=1 close_write=1',
        'NATIVE_STATVFS files=0 ffree=0 favail=0 blocks=1024 bfree=1000 bavail=1000 frsize=4096',
        f'LOCI_BTRFS_WORKLOAD_PASS run_id={RUN_ID}',
        f'LOCI_BTRFS_COMPLETE run_id={RUN_ID}',
    ]) + '\n'


class RetainedLogCLI(unittest.TestCase):
    def classify(self, body, *extra, exit_status=0):
        with tempfile.TemporaryDirectory() as temp:
            log = Path(temp) / 'serial.log'
            log.write_text(body)
            result = subprocess.run([sys.executable, str(RUNNER), 'classify-log', '--log', str(log),
                                     '--sha', SHA, '--content-sha256', CONTENT, '--run-id', RUN_ID,
                                     '--exit-status', str(exit_status), *extra], capture_output=True, text=True)
            return result.returncode, json.loads(result.stdout)

    def test_zero_only_shutdown_and_old_capability_markers_cannot_pass(self):
        code, result = self.classify('GUEST_CAPABILITY_PASS\nGUEST_WORKLOAD_PASS\n')
        self.assertEqual(code, 1)
        self.assertFalse(result['engine_acceptance'])

    def test_complete_synthetic_log_preserves_unavailable_btrfs_inode_metric(self):
        code, result = self.classify(passing_lines())
        self.assertEqual(code, 0)
        self.assertEqual(result['inode_capacity']['status'], 'unavailable')
        self.assertFalse(result['inode_capacity']['metadata_capacity_verified'])
        self.assertFalse(result['issue16_resolved'])

    def test_wrong_receipt_and_wrong_run_token_cannot_pass(self):
        for body in (passing_lines().replace(CONTENT, 'c' * 64), passing_lines().replace(RUN_ID, 'old-run'), passing_lines().replace('inotify_bytes=64', 'inotify_bytes=0')):
            self.assertEqual(self.classify(body)[0], 1)

    def test_skip_reason_is_retained_and_timeout_cannot_pass(self):
        code, result = self.classify(passing_lines() + 'UNVERIFIED: metadata pilot unavailable\n')
        self.assertEqual(code, 1)
        self.assertIn('metadata pilot unavailable', result['failure_or_unverified_lines'][0])
        self.assertEqual(self.classify(passing_lines(), '--timed-out')[0], 1)


class LaunchSafetyCLI(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.base = Path(self.temp.name)
        self.repo = self.base / 'source'
        self.repo.mkdir()
        def git(*args):
            return subprocess.check_output(['git', '-C', str(self.repo), *args], text=True).strip()
        git('init', '-q')
        (self.repo / 'tracked').write_text('synthetic process-safety fixture\n')
        git('add', 'tracked')
        git('-c', 'user.name=Safety Fixture', '-c', 'user.email=fixture@example.invalid',
            'commit', '-qm', 'Synthetic safety fixture')
        self.sha = git('rev-parse', 'HEAD')
        self.prefix = self.base / 'owned'
        self.prefix.mkdir()
        for name in ('kernel', 'initramfs', 'artifact.img', 'cli', 'test'):
            (self.prefix / name).write_bytes(b'synthetic artifact, never booted\n')
        self.data = self.prefix / 'data.img'
        self.data.write_bytes(b'\0' * 4096)
        self.qemu = self.prefix / 'standin-process'
        self.qemu.write_text(f'#!{sys.executable}\nimport time\ntime.sleep(300)\n')
        self.qemu.chmod(0o755)
        sort = Path(shutil.which('sort'))
        build = {'source': {'path': str(self.repo), 'sha': self.sha}, 'commands': [['synthetic-fixture']],
                 'rustc_version': 'synthetic safety fixture, not compiler evidence', 'profiles': ['debug'],
                 'test_artifacts': [{'path': str(self.prefix / 'test'), 'sha256': digest(self.prefix / 'test')}],
                 'cli_artifacts': [{'path': str(self.prefix / 'cli'), 'sha256': digest(self.prefix / 'cli')}],
                 'drivers': [], 'runtime_overlay': [{'snapshot': str(sort), 'container_path': '/usr/bin/sort', 'sha256': digest(sort)}]}
        compile_log = self.prefix / 'build-synthetic.log'
        compile_log.write_text('Synthetic fixture only; no compiler executed.\n')
        build['build_log_digests'] = {compile_log.name: digest(compile_log)}
        self.build = self.prefix / 'build.json'
        self.build.write_text(json.dumps(build))
        content = {'source_sha': self.sha, 'artifacts': [
            {'guest_path': item['path'], 'sha256': item['sha256']} for item in build['test_artifacts'] + build['cli_artifacts']
        ] + [{'guest_path': '/usr/bin/sort', 'sha256': digest(sort)}]}
        self.content = self.prefix / 'content.json'
        self.content.write_text(json.dumps(content))
        st = self.data.stat()
        owner = {'schema': 'loci.btrfs-owned-data.v1', 'path': str(self.data), 'uid': os.geteuid(),
                 'device': st.st_dev, 'inode': st.st_ino, 'run_id': RUN_ID}
        self.owner = self.prefix / 'owner.json'
        self.owner.write_text(json.dumps(owner))
        self.manifest = self.prefix / 'guest.json'
        self.document = {'schema': 'loci.btrfs-guest-input.v1', 'source': {'sha': self.sha},
                         'sha256': {'qemu': digest(self.qemu), 'kernel': digest(self.prefix / 'kernel'),
                                    'initramfs': digest(self.prefix / 'initramfs'), 'artifact_image': digest(self.prefix / 'artifact.img')},
                         'guest_content_manifest_sha256': digest(self.content), 'guest_content_manifest': str(self.content),
                         'build_manifest': {'path': str(self.build), 'sha256': digest(self.build)},
                         'data_image_owner_receipt': {'path': str(self.owner), 'sha256': digest(self.owner)},
                         'btrfs_budget': {'metadata_profile': 'DUP', 'metadata_copies': 2,
                                          'data_bytes': 100, 'metadata_logical_bytes': 100, 'sort_and_logs_bytes': 100}}
        self.write_manifest()

    def write_manifest(self):
        self.manifest.write_text(json.dumps(self.document))

    def tearDown(self):
        self.temp.cleanup()

    def command(self, data=None):
        return [sys.executable, str(RUNNER), 'run', '--owned-prefix', str(self.prefix),
                '--source', str(self.repo), '--sha', self.sha, '--qemu', str(self.qemu),
                '--kernel', str(self.prefix / 'kernel'), '--initramfs', str(self.prefix / 'initramfs'),
                '--artifact-image', str(self.prefix / 'artifact.img'), '--data-image', str(data or self.data),
                '--artifact-manifest', str(self.manifest), '--results', str(self.prefix / 'results'),
                '--run-id', RUN_ID, '--timeout', '1', '--max-log-bytes', '4096', '--host-write-budget-bytes', '8192']

    def test_symlink_writable_disk_and_missing_ownership_are_rejected_before_launch(self):
        link = self.prefix / 'alias.img'
        link.symlink_to(self.data)
        result = subprocess.run(self.command(link), capture_output=True, text=True)
        self.assertEqual(result.returncode, 1)
        self.assertIn('symlink', result.stderr)
        self.document.pop('data_image_owner_receipt')
        self.write_manifest()
        result = subprocess.run(self.command(), capture_output=True, text=True)
        self.assertEqual(result.returncode, 1)
        self.assertFalse((self.prefix / 'results').exists())

    @unittest.skipUnless(hasattr(os, 'pidfd_open'), 'Linux held-pidfd timeout seam unavailable')
    def test_timeout_kills_only_owned_standin_and_preserves_unrelated_process(self):
        decoy = subprocess.Popen([str(self.qemu)], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        try:
            result = subprocess.run(self.command(), capture_output=True, text=True, timeout=10)
            self.assertEqual(result.returncode, 1, result.stderr)
            summary = json.loads((self.prefix / 'results' / 'summary.json').read_text())
            self.assertTrue(summary['timed_out'])
            self.assertTrue(summary['termination']['root_exit_confirmed'])
            self.assertFalse(summary['termination']['descendant_release_confirmed'])
            self.assertIsNone(decoy.poll())
        finally:
            decoy.terminate()
            decoy.wait(timeout=3)


if __name__ == '__main__':
    unittest.main()
