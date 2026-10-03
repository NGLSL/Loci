"""Public CLI and real pipe/receipt tests; no Engine or 24h acceptance claim."""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

HERE = Path(__file__).resolve().parent

class ControllerCLI(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.base = Path(self.temp.name)
        self.root = self.base / 'fixture'
        (self.root / 'dir00000').mkdir(parents=True)
        self.original = self.root / 'dir00000/original'
        self.original.write_bytes(b'keep')
        self.run = self.base / 'evidence'
        self.run.mkdir()
        self.config = self.base / 'config.json'
        self.cfg = {'sha': 'a' * 40,
                    'worker_binary_sha256': hashlib.sha256(Path(sys.executable).read_bytes()).hexdigest(),
                    'worker_command': [sys.executable, str(HERE / 'fake_protocol_worker.py'), 'wrong-id', 'a' * 40],
                    'root': str(self.root), 'database': str(self.run / 'state.db'),
                    'ipc_timeout_seconds': 1, 'oracle_timeout_seconds': 2,
                    'export_budget_bytes': 1024, 'artifact_budget_bytes': 64 * 1024 * 1024}

    def call(self, action, resume=False):
        self.config.write_text(json.dumps(self.cfg))
        return subprocess.run([sys.executable, str(HERE / 'soak_controller.py'), action,
                               '--config', str(self.config), '--run-dir', str(self.run),
                               *(['--resume'] if resume else [])], capture_output=True, text=True, timeout=15)

    def test_preparation_creates_reviewable_receipt_without_starting_worker_or_passing_day(self):
        result = self.call('prepare')
        self.assertEqual(result.returncode, 0, result.stderr)
        receipt = json.loads((self.run / 'preparation.json').read_text())
        self.assertEqual(receipt['status'], 'prepared-not-run')
        self.assertEqual(receipt['requested_seconds'], 86400)
        self.assertFalse(receipt['engine_acceptance'])
        self.assertFalse((self.run / 'run.json').exists())
        self.assertFalse((self.run / 'state.db').exists())
        self.assertEqual(self.original.read_bytes(), b'keep')

    def test_resource_gates_cannot_be_relaxed_in_cli_input(self):
        for key, value in [('quiet_cpu_limit_percent', 2), ('steady_rss_limit_bytes', 201*1024*1024),
                           ('peak_rss_limit_bytes', 513*1024*1024)]:
            with self.subTest(key=key):
                self.cfg[key] = value
                result = self.call('prepare')
                self.assertNotEqual(result.returncode, 0)
                self.assertIn('resource gate cannot be relaxed', result.stderr)
                self.assertFalse((self.run / 'preparation.json').exists())
                del self.cfg[key]

    def test_bad_external_worker_sequence_and_oversize_keep_failure_artifacts(self):
        for mode in ('wrong-id', 'oversize'):
            with self.subTest(mode=mode):
                self.cfg['worker_command'][2] = mode
                result = self.call('protocol-smoke', resume=(self.run/'run.json').exists())
                self.assertNotEqual(result.returncode, 0, result.stdout)
                receipt = json.loads((self.run/'run.json').read_text())
                self.assertFalse(receipt['engine_acceptance'])
                self.assertIn('failed', receipt['status'])
                self.assertTrue(list(self.run.glob('window-*/logs/*')))
                self.assertEqual(self.original.read_bytes(), b'keep')

    def test_resume_rejects_different_binary_and_unproven_mutation_is_retained(self):
        self.assertNotEqual(self.call('protocol-smoke').returncode, 0)
        prior = json.loads((self.run/'run.json').read_text())
        uncertain = self.root/'dir00000/uncertain'
        uncertain.write_bytes(b'evidence')
        info = self.root.stat()
        receipt = {'schema': 1, 'run_id': prior['run_id'], 'root_hex': os.fsencode(self.root).hex(),
                   'root_identity': [info.st_dev, info.st_ino], 'created': {os.fsencode(uncertain).hex(): None},
                   'rename': None, 'permission': None, 'file_rename': None}
        (self.run/'mutation-receipt.json').write_text(json.dumps(receipt))
        result = self.call('protocol-smoke', resume=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(uncertain.read_bytes(), b'evidence')
        self.assertIn('unproven object ownership', (self.run/'run.json').read_text())
        self.cfg['worker_binary_sha256'] = 'c' * 64
        result = self.call('protocol-smoke', resume=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(uncertain.read_bytes(), b'evidence')

if __name__ == '__main__':
    unittest.main()
