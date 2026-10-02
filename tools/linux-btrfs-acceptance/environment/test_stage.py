"""Small source/mapping/parser checks only: no guest, compiler or native acceptance."""
import gzip
import importlib.util
import pathlib
import subprocess
import tempfile
import unittest
HERE=pathlib.Path(__file__).resolve().parent
spec=importlib.util.spec_from_file_location('btrfs_stage',HERE/'stage.py');stage=importlib.util.module_from_spec(spec);spec.loader.exec_module(stage)

class MappingAndArchiveTests(unittest.TestCase):
    def test_exact_absolute_cli_runtime_paths_stay_unchanged(self):
        for value in ['/workspace/frozen-artifacts/target/debug/loci-experiment','/usr/bin/sort','/lib64/ld-linux-x86-64.so.2','/guest/source/native-probe.c']:
            self.assertEqual(stage.guest_destination(value),value)
        for value in ['relative','/x/./y','/x//y','/x/../y','/proc/self/exe','/dev/vda','/artifacts/a','/x\nmessage','/x space','/x"quote']:
            with self.assertRaises(ValueError):stage.guest_destination(value)

    def test_newc_assembly_preserves_symlink_and_declares_console_without_host_device(self):
        with tempfile.TemporaryDirectory() as d:
            root=pathlib.Path(d)/'tree';root.mkdir();(root/'bin').mkdir();(root/'bin/tool').write_bytes(b'tiny');(root/'bin/link').symlink_to('tool')
            archive=pathlib.Path(d)/'initramfs.cpio.gz';stage.archive_initramfs(root,archive)
            raw=gzip.decompress(archive.read_bytes())
            self.assertTrue(raw.startswith(b'070701'));self.assertIn(b'bin/link\0',raw);self.assertIn(b'dev/console\0',raw);self.assertIn(b'TRAILER!!!\0',raw);self.assertFalse((root/'dev').exists())
            with self.assertRaises(FileExistsError):stage.archive_initramfs(root,archive)

class UsageParserTests(unittest.TestCase):
    # Synthetic parser inputs only; never native capacity or Engine evidence.
    usage='''Overall:
    Device size: 8589934592
    Device allocated: 562036736
    Device unallocated: 8027897856
    Device missing: 0
    Free (estimated): 8000000000 (min: 4000000000)
Data,single: Size:8388608, Used:100
Metadata,DUP: Size:268435456, Used:1000
System,DUP: Size:8388608, Used:16384
'''
    def parse(self,text,reserve):
        with tempfile.TemporaryDirectory() as d:
            out=pathlib.Path(d)/'physical.txt'
            p=subprocess.run(['awk','-v',f'reserve={reserve}','-v',f'physical_out={out}','-f',str(HERE/'usage_guard.awk')],input=text,text=True,stdout=subprocess.PIPE,stderr=subprocess.PIPE)
            return p.returncode,p.stdout,out.read_text() if out.exists() else None
    def test_actual_fields_use_two_metadata_copies_and_record_unavailable_inodes(self):
        code,body,physical=self.parse(self.usage,128*1024*1024)
        self.assertEqual(code,0);self.assertEqual(physical,'2100\n');self.assertIn('metadata_physical_used=2000',body);self.assertIn('inode_capacity=unavailable_dynamic',body)
    def test_unknown_profiles_missing_measurements_or_insufficient_reserve_fail(self):
        for text,reserve in [(self.usage.replace('Metadata,DUP:','Metadata,single:'),1),(self.usage.replace('Device unallocated:','Missing unknown:'),1),(self.usage.replace('Used:1000','Used:unknown'),1),(self.usage,5*1024*1024*1024)]:
            code,body,_=self.parse(text,reserve);self.assertNotEqual(code,0);self.assertIn('LOCI_BTRFS_FAIL',body)

if __name__=='__main__':unittest.main()
