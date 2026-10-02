"""Small public-CLI checks; never build or run native fixtures."""
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

RUNNER = Path(__file__).with_name('runner.py')


class LogClassificationCLI(unittest.TestCase):
    def test_environment_skip_is_named_and_removed_from_pass_count(self):
        with tempfile.TemporaryDirectory() as work:
            log = Path(work) / 'native.log'
            log.write_text('test native_bind_mount_scope ... UNVERIFIED: private namespace unavailable\n'
                           'ok\ntest actual_native_case ... ok\n'
                           'test result: ok. 2 passed; 0 failed; 0 ignored;\n')
            result = subprocess.run([sys.executable, str(RUNNER), 'classify-log',
                                     '--log', str(log), '--exit-status', '0'],
                                    capture_output=True, text=True)
            self.assertEqual(result.returncode, 4, result.stderr)
            output = json.loads(result.stdout)
            self.assertEqual(output['status'], 'unverified')
            self.assertEqual(output['passed_test_count_without_environment_skips'], 1)
            self.assertEqual(output['environment_unverified'][0]['test'], 'native_bind_mount_scope')
            self.assertEqual(output['environment_unverified'][0]['reason'], 'private namespace unavailable')


    def classify(self, text, code=0, extra=()):
        with tempfile.TemporaryDirectory() as work:
            log = Path(work) / 'native.log'
            log.write_text(text)
            result = subprocess.run([sys.executable, str(RUNNER), 'classify-log',
                                     '--log', str(log), '--exit-status', str(code), *extra],
                                    capture_output=True, text=True)
            return result.returncode, json.loads(result.stdout)

    def test_zero_exit_without_actual_overflow_marker_fails(self):
        code, result = self.classify('test actual_overflow ... ok\n'
                                    'test result: ok. 1 passed; 0 failed; 0 ignored;\n',
                                    extra=('--required-one-test', '--required-marker',
                                           'native_scale_kernel_overflow_verified'))
        self.assertEqual(code, 1)
        self.assertEqual(result['status'], 'failed')
        self.assertIn('positive native proof marker missing', result['failure_reasons'][0])

    def test_native_proof_with_a_safety_skip_stays_unverified(self):
        code, result = self.classify('test native_overflow ... SKIP: queue exceeds safety budget\n'
                                    'ok\ntest result: ok. 1 passed; 0 failed; 0 ignored;\n',
                                    extra=('--required-one-test', '--required-marker', 'positive marker'))
        self.assertEqual(code, 4)
        self.assertEqual(result['passed_test_count_without_environment_skips'], 0)
        self.assertEqual(result['environment_unverified'][0]['test'], 'native_overflow')
        self.assertEqual(result['environment_unverified'][0]['reason'], 'queue exceeds safety budget')

    def test_timeout_cannot_pass_from_an_earlier_success_summary(self):
        code, result = self.classify('test case ... ok\n'
                                    'test result: ok. 1 passed; 0 failed; 0 ignored;\n', code=124)
        self.assertEqual(code, 1)
        self.assertEqual(result['status'], 'failed')
        self.assertIn('exit_status=124', result['failure_reasons'])

    def test_ignored_case_is_retained_without_credit(self):
        code, result = self.classify('test real100k ... ignored, owned large fixture opt-in\n'
                                    'test result: ok. 0 passed; 0 failed; 1 ignored;\n')
        self.assertEqual(code, 0)
        self.assertEqual(result['passed_test_count_without_environment_skips'], 0)
        self.assertEqual(result['ignored_not_passed'],
                         [{'test': 'real100k', 'reason': 'ignored, owned large fixture opt-in'}])

    def test_short_sha_is_rejected_before_build_or_artifact_creation(self):
        with tempfile.TemporaryDirectory() as work:
            artifacts = Path(work) / 'artifacts'
            result = subprocess.run([sys.executable, str(RUNNER), 'prepare',
                                     '--source', work, '--sha', 'abcdef1',
                                     '--artifacts', str(artifacts)], capture_output=True, text=True)
            self.assertEqual(result.returncode, 2)
            self.assertIn('full lowercase 40-hex commit ID', result.stderr)
            self.assertFalse(artifacts.exists())


if __name__ == '__main__':
    unittest.main()
