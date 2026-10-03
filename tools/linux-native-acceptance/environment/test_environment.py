"""Public staging/print-plan safety checks only; no native setup or image build."""
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

HERE = Path(__file__).resolve().parent


class EnvironmentCLI(unittest.TestCase):
    def test_plan_does_not_execute_workload_or_create_owned_paths(self):
        with tempfile.TemporaryDirectory() as temp:
            base = Path(temp)
            prefix, mount, marker = (base / name for name in ('fresh-prefix', 'fresh-mount', 'marker'))
            result = subprocess.run(['bash', str(HERE / 'run-ext4.sh'), '--owned-prefix', str(prefix),
                                     '--mountpoint', str(mount), '--print-plan', '--', '/usr/bin/touch', str(marker)],
                                    capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn('reserve_percent=15 uid=1000 gid=1000', result.stdout)
            self.assertFalse(prefix.exists())
            self.assertFalse(mount.exists())
            self.assertFalse(marker.exists())

    def test_existing_owned_prefix_is_rejected_even_in_plan_mode(self):
        with tempfile.TemporaryDirectory() as temp:
            result = subprocess.run(['bash', str(HERE / 'run-ext4.sh'), '--owned-prefix', temp,
                                     '--mountpoint', str(Path(temp) / 'fresh'), '--print-plan', '--', '/usr/bin/true'],
                                    capture_output=True, text=True)
            self.assertEqual(result.returncode, 2)
            self.assertIn('must not already exist', result.stderr)

    def stage_invalid(self, destination, checksum):
        with tempfile.TemporaryDirectory() as temp:
            base = Path(temp)
            (base / 'input').write_text('tiny synthetic dependency\n')
            manifest = base / 'manifest.json'
            manifest.write_text(json.dumps({'schema': 'loci.ext4-environment-input.v1', 'files': [
                {'source': 'input', 'destination': destination, 'sha256': checksum}]}))
            output = base / 'output'
            result = subprocess.run([sys.executable, str(HERE / 'stage-rootfs.py'), '--input-root', str(base),
                                     '--input-manifest', str(manifest), '--output-owned', str(output)],
                                    capture_output=True, text=True)
            self.assertEqual(result.returncode, 1)
            self.assertFalse(output.exists())
            return json.loads(result.stderr)['error']

    def test_escaping_destination_is_rejected_before_any_context_write(self):
        self.assertIn('canonical absolute', self.stage_invalid('/../../escape', '0' * 64))

    def test_unverified_dependency_bytes_are_rejected_before_any_context_write(self):
        self.assertIn('matching digest', self.stage_invalid('/usr/bin/bash', '0' * 64))


if __name__ == '__main__':
    unittest.main()
