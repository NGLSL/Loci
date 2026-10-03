"""Public source stager/parser CLI checks; no guest or native acceptance."""
import gzip
import json
import os
import stat
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

class RuntimeMetadataCLI(unittest.TestCase):
    @unittest.skipUnless(os.environ.get('LOCI_BTRFS_STAGE_INPUTS'),
                         'explicit owned native build/environment inputs required')
    def test_stage_only_under_private_umask_makes_only_guest_mappings_public(self):
        inputs = json.loads(Path(os.environ['LOCI_BTRFS_STAGE_INPUTS']).read_text())
        output = Path(inputs['output'])
        private = Path(inputs['base_initramfs']) / 'etc/shadow'
        source_mode = stat.S_IMODE(private.stat().st_mode)
        argv = [sys.executable, str(HERE/'stage.py'), 'plan', '--stage-only']
        for key in ['output', 'owned_prefix', 'source', 'sha', 'build_manifest',
                    'qemu', 'kernel', 'base_initramfs', 'fs_tools', 'run_id', 'rust_env']:
            argv += ['--'+key.replace('_', '-'), inputs[key]]
        result = subprocess.run(argv, capture_output=True, text=True, timeout=120,
                                preexec_fn=lambda: os.umask(0o077))
        self.assertEqual(result.returncode, 0, result.stderr)
        receipt = json.loads((output/'runtime-stage.json').read_text())
        self.assertEqual(receipt['status'], 'runtime_staged_not_assembled')
        self.assertFalse(receipt['engine_acceptance'])
        self.assertFalse(receipt['actual_guest_execution'])
        for image in ['data.btrfs', 'artifacts.ext4', 'guest-manifest.json']:
            self.assertFalse((output/image).exists())
        self.assertEqual(stat.S_IMODE(private.stat().st_mode), source_mode)
        self.assertEqual(stat.S_IMODE((output/'initramfs/etc/shadow').stat().st_mode), source_mode)
        self.assertEqual(stat.S_IMODE((output/'artifact-root').stat().st_mode), 0o755)
        for path in ['artifact-root/guest', 'initramfs/guest', 'initramfs/workspace']:
            self.assertEqual(stat.S_IMODE((output/path).stat().st_mode), 0o755)
        for path in ['guest/native-probe', 'guest/fixture', 'guest/btrfs-matrix']:
            self.assertEqual(stat.S_IMODE((output/'artifact-root'/path).stat().st_mode), 0o755)
        self.assertEqual(stat.S_IMODE((output/'artifact-root/guest/readonly-proof').stat().st_mode), 0o666)
        # Independent newc format reader: metadata emitted to the guest, rather
        # than private stager functions or inferred host-user access.
        raw = gzip.decompress((output/'initramfs.cpio.gz').read_bytes())
        cursor, entries = 0, {}
        while cursor < len(raw):
            header = raw[cursor:cursor+110]
            self.assertEqual(header[:6], b'070701')
            fields = [int(header[6+n*8:14+n*8], 16) for n in range(13)]
            cursor += 110
            name = raw[cursor:cursor+fields[11]-1].decode()
            cursor = (cursor+fields[11]+3)//4*4
            entries[name] = fields
            cursor = (cursor+fields[6]+3)//4*4
            if name == 'TRAILER!!!':
                break
        for name in ['.', 'guest', 'workspace', 'guest/source']:
            self.assertEqual(entries[name][1] & 0o777, 0o755)
            self.assertEqual(entries[name][2:4], [0, 0])

if __name__ == '__main__':
    unittest.main()
