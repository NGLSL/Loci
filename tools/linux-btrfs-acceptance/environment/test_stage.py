"""Public source stager/parser CLI checks; no guest or native acceptance."""
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
HERE = Path(__file__).resolve().parent

class StagerCLI(unittest.TestCase):
    def test_source_only_receipt_does_not_claim_assembly_or_engine_acceptance(self):
        with tempfile.TemporaryDirectory() as directory:
            out = Path(directory)/'preparation.json'
            command = [sys.executable, str(HERE/'stage.py'), 'source-only', '--output', str(out)]
            result = subprocess.run(command, capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            receipt = json.loads(out.read_text())
            self.assertEqual(receipt['status'], 'source_only_not_assembled')
            self.assertFalse(receipt['engine_acceptance'])
            self.assertFalse(receipt['actual_guest_execution'])
            self.assertIsNone(receipt['final_sha'])
            saved = out.read_bytes()
            self.assertNotEqual(subprocess.run(command, capture_output=True).returncode, 0)
            self.assertEqual(out.read_bytes(), saved)

    def test_plan_requires_final_source_and_environment_receipts(self):
        with tempfile.TemporaryDirectory() as directory:
            out = Path(directory)/'plan.json'
            result = subprocess.run([sys.executable, str(HERE/'stage.py'), 'plan', '--output', str(out)], capture_output=True, text=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('explicit final source/build/private environment/run token inputs required', result.stderr)
            self.assertFalse(out.exists())

    def test_physical_capacity_cli_requires_dup_and_measured_reserve(self):
        usage = '''Overall:
    Device size: 8589934592
    Device allocated: 562036736
    Device unallocated: 8027897856
    Device missing: 0
    Free (estimated): 8000000000 (min: 4000000000)
Data,single: Size:8388608, Used:100
Metadata,DUP: Size:268435456, Used:1000
System,DUP: Size:8388608, Used:16384
'''
        with tempfile.TemporaryDirectory() as directory:
            out = Path(directory)/'physical.txt'
            argv = ['awk', '-v', 'reserve=134217728', '-v', f'physical_out={out}', '-f', str(HERE/'usage_guard.awk')]
            result = subprocess.run(argv, input=usage, text=True, capture_output=True)
            self.assertEqual(result.returncode, 0, result.stdout)
            self.assertEqual(out.read_text(), '2100\n')
            for damaged in [usage.replace('Metadata,DUP:', 'Metadata,single:'), usage.replace('Device unallocated:', 'Missing unknown:')]:
                self.assertNotEqual(subprocess.run(argv, input=damaged, text=True, capture_output=True).returncode, 0)

if __name__ == '__main__':
    unittest.main()
