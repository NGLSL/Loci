#!/usr/bin/env python3
"""Copy an explicit, hashed environment dependency receipt into a fresh owned context."""
import argparse
import hashlib
import json
from pathlib import Path, PurePosixPath
import shutil
import sys

REQUIRED = {
    '/usr/bin/bash', '/usr/bin/cat', '/usr/bin/chown', '/usr/bin/df',
    '/usr/bin/findmnt', '/usr/bin/id', '/usr/bin/mkdir', '/usr/bin/mknod',
    '/usr/bin/mount', '/usr/bin/readlink', '/usr/bin/rm', '/usr/bin/rmdir',
    '/usr/bin/setpriv', '/usr/bin/stat', '/usr/bin/truncate', '/usr/bin/umount',
    '/usr/bin/timeout', '/usr/bin/unshare', '/usr/bin/cp', '/usr/bin/sha256sum',
    '/usr/sbin/losetup', '/usr/sbin/mke2fs', '/etc/mke2fs.conf',
    '/bin/sh', '/usr/bin/sh', '/usr/bin/sync',
    '/lib64/ld-linux-x86-64.so.2', '/lib/x86_64-linux-gnu/libc.so.6',
}


def digest(path):
    value = hashlib.sha256()
    with Path(path).open('rb') as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b''):
            value.update(chunk)
    return value.hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--input-root', type=Path, required=True)
    parser.add_argument('--input-manifest', type=Path, required=True)
    parser.add_argument('--output-owned', type=Path, required=True)
    args = parser.parse_args()
    source_root = args.input_root.resolve(strict=True)
    document = json.loads(args.input_manifest.read_text())
    if document.get('schema') != 'loci.ext4-environment-input.v1':
        raise ValueError('explicit environment dependency manifest required')
    files = document.get('files', [])
    inputs = []
    destinations = set()
    reserved = {'/usr/bin/loci-run-ext4', '/etc/passwd', '/etc/group'}
    for entry in files:
        destination = PurePosixPath(entry['destination'])
        if not destination.is_absolute() or '..' in destination.parts or str(destination) == '/':
            raise ValueError('canonical absolute dependency destinations required')
        if str(destination) in destinations or str(destination) in reserved:
            raise ValueError('duplicate or reserved dependency destination')
        destinations.add(str(destination))
        source = (source_root / entry['source']).resolve(strict=True)
        source.relative_to(source_root)
        if not source.is_file() or entry['sha256'] != digest(source):
            raise ValueError('regular input file with matching digest required')
        inputs.append((source, destination, entry['sha256']))
    if not REQUIRED.issubset(destinations):
        raise ValueError('missing required tools/config: ' + ', '.join(sorted(REQUIRED - destinations)))
    output = args.output_owned.absolute()
    if output.exists() or output.is_symlink():
        raise ValueError('output context must be fresh; existing data is never overwritten')
    output.mkdir(mode=0o700)
    rootfs = output / 'rootfs'
    rootfs.mkdir()
    for name in ('tmp', 'mnt', 'root', 'dev', 'proc', 'sys', 'run'):
        (rootfs / name).mkdir()
    for source, destination, expected in inputs:
        target = rootfs / str(destination).lstrip('/')
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(source, target)
        if digest(target) != expected:
            raise ValueError('input changed during copy; partial context retained for inspection')
    here = Path(__file__).resolve().parent
    helper = rootfs / 'usr/bin/loci-run-ext4'
    helper.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(here / 'run-ext4.sh', helper)
    helper.chmod(0o755)
    for name in ('passwd', 'group'):
        target = rootfs / 'etc' / name
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(here / name, target)
    shutil.copy2(here / 'Dockerfile', output / 'Dockerfile')
    # Rootfs files are readable by the runtime's UID1000. Ownership in the final
    # scratch image is assigned by Docker COPY; no host chown or install occurs.
    for path in rootfs.rglob('*'):
        path.chmod(0o755 if path.is_dir() or path.stat().st_mode & 0o111 else 0o644)
    (rootfs / 'tmp').chmod(0o1777)
    receipt = {'schema': 'loci.ext4-environment-context.v1', 'status': 'staged-not-built',
               'engine_acceptance': False, 'issue15_resolved': False,
               'input_manifest_sha256': digest(args.input_manifest),
               'source_glue_sha256': {name: digest(here / name) for name in ('run-ext4.sh', 'Dockerfile', 'passwd', 'group', 'stage-rootfs.py')},
               'files': {str(path.relative_to(rootfs)): digest(path) for path in rootfs.rglob('*') if path.is_file()}}
    (output / 'environment-manifest.json').write_text(json.dumps(receipt, indent=2) + '\n')
    print(json.dumps({'status': 'staged-not-built', 'context': str(output), 'engine_acceptance': False}))


if __name__ == '__main__':
    try:
        main()
    except (OSError, ValueError, KeyError) as exc:
        print(json.dumps({'status': 'failed', 'error': str(exc)}), file=sys.stderr)
        raise SystemExit(1)
