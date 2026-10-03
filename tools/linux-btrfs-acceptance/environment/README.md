# Source-only Btrfs guest environment glue

These source files supply the assembler/init/probe/fixture/matrix/workload
entry needed by the parent checked launcher. They have not assembled or executed
a final guest. No final SHA, kernel, modules, packages, QEMU, binaries, images or
old exploratory evidence is bundled here. Source preparation is not Task16/17
acceptance. Matching stock guest kernel/modules, a compatible BusyBox initramfs,
QEMU/firmware/private libraries and filesystem formatters remain supplied inputs.

The assembler consumes the final shared Linux runner build manifest and immutable
`stage_tools` GNU sort/loader/library overlay. It preserves all compiled absolute
CLI/test/driver destinations with read-only artifact bindings, verifies source
and build logs, and records environment helper source/compiler/image digests.
It never downloads, globally installs tools, loads host modules, changes sysctl,
starts a network guest or uses a host block device. `plan` validates and writes a
recipe; only its explicit `--assemble` compiles small helpers and creates fresh
regular owned images. It does not start QEMU. Images/results belong outside Git.

An explicit8GiB sparse Btrfs image is provisioned with an exact path/uid/dev/inode/
run-token ownership receipt. Assembly reserves its entire possible growth,
artifact image and logs while keeping15% host backing free. The guest records
raw `btrfs filesystem usage -b`; a real100k physical metadata DUP pilot precedes
million creation with1.5x measured physical growth projection. Unknown profiles,
insufficient minfree/device/statvfs reserve or failed pilot stops. f_files=0 is
unavailable dynamic capacity, never an invented fixed inode count. Basenames and
directory depth vary;1M means20,000 directories plus980,000 actual regular files.

UID/GID1000 probe verifies Btrfs magic, positive statx mount ID/inode, real inotify
create/close-write and errnoEROFS on the actual read-only ext4 artifact volume.
The guest independently hashes content.json and every mounted content entry
before SOURCE is printed. The small matrix uses the final CLI only, including
subvolumes/inode256 reuse, rawbytes/links, mount/bind boundaries, rename/followup
and permission gaps. Debug/release native jobs retain full raw logs and require
real libtest summaries/positive overflow markers; SKIP/UNVERIFIED is not credited.
Native test TMPDIR is explicitly placed on the same Btrfs data volume. Before each native job, root captures fresh detailed usage in a root-owned read-only receipt bound to canonical workdir/device/inode/mount ID, raw payload hash and source SHA; Engine/permission execution remains UID1000.

The coordinated testside Btrfs budget helper consumes `LOCI_BTRFS_CAPACITY_RECEIPT` and `LOCI_TEST_SOURCE_SHA`. An older fixed-inode test may reject Btrfs until that correction is included in the frozen source; it remains a visible failure, never a synthetic pass. Helper C/Rust compilation, actual mapping/boot,
metadata pilot and final native/full-oracle jobs remain unverified until run.

The public1M driver executes functional correctness/stages/restart separately,
with its independent complete raw-byte oracles. The100k pilot uses `--smoke`
explicitly: actual100k proof, never million/reference acceptance. Guest TCG2CPU/
2048MiB is not referenceSSD/4CPU16GiB. Timing gates remain separately unverified;
any later timing phase retains the full52-line query suite/200 repetitions and
original thresholds. Do not lower deadlines or treat reference timing failure
as a functional oracle result. Parent wrapper pass cannot directly become the
Task17 passed native-behavior prerequisite receipt.

All native logs/manifests/oracles/failures stay in the host-persistent declared
data image under `/mnt/data/TOKEN/results`. Serial carries bounded source/token/
content/UID/filesystem and per-job exit/hash receipts. Successful duplicate
NUL/kind/sort files are hashed then removed within a2GiB aggregate checkpoint/
results budget; failures stop with raw data retained. The raw image must survive
`--rm`/session termination. A separate read-only guest result extraction and host
log-classifier assessment is still required; it is an explicit remaining runtime
handoff, not presumed available because source files now exist.

## Future frozen-artifact commands

Use real supplied paths and compiler/private ABI inputs. These placeholders are
not a final source or execution claim. Output paths must be fresh and owned.

```sh
python3 tools/linux-btrfs-acceptance/environment/stage.py source-only \
  --output /owned/preparation/source-glue.json

python3 tools/linux-btrfs-acceptance/environment/stage.py plan \
  --owned-prefix /owned --source /owned/frozen-source --sha FROZEN_40_SHA \
  --build-manifest /owned/frozen-build/manifest.json \
  --qemu /owned/environment/bin/qemu-system-x86_64 \
  --kernel /owned/environment/kernel \
  --base-initramfs /owned/environment/initramfs-tree \
  --fs-tools /owned/environment/fs-tools-root \
  --cc /supplied/compiler/cc --rustc rustc \
  --rust-env /supplied/rust-env.sh --run-id UNIQUE_TOKEN \
  --output /owned/preparation/guest-plan.json
```

`fs-tools-root` supplies its private `usr/bin/btrfs`, `usr/sbin/mke2fs`,
`usr/sbin/mkfs.btrfs`, compatible `usr/lib/x86_64-linux-gnu`, and
`etc/mke2fs.conf`. The base initramfs tree supplies static BusyBox applet links,
matching guest kernel modules, `/etc` UID/GID1000 probe account, and the private
Btrfs/compiled ELF ABI dependency closure. These are explicit environment inputs,
not a second package/runtime resolver; GNU sort still comes from shared stage_tools.
`--rust-env` is optional if supplied PATH already provides the chosen rustc.

After recipe/budget review and an isolated scheduled window, change output to a
fresh directory and add `--assemble`. It produces `assembly.json`, outer
`guest-manifest.json`, content mapping, exact data-owner receipt, compiler logs,
initramfs and regular artifact/data images. Source/image identity is checked;
outer checked runner owns bounded actual launch and post-run classification.
Use parent README flags and measured assembly budgets. Do not execute any
assembly/VM/fixture while CPU/latency/quiet measurements are active.

## Small checks

```sh
python3 -m unittest discover \
  -s tools/linux-btrfs-acceptance/environment -p 'test_*.py'
python3 -m py_compile tools/linux-btrfs-acceptance/environment/stage.py
```

They check exact mapping, newc console/symlink serialization and explicitly
synthetic DUP usage parsing. They invoke no compiler, QEMU, image formatter,
fixture generator or native Engine. C/Rust helpers require separate compilation
and real guest proof after the parent schedules it.

## Native budget receipt contract

Root writes a regular mode0644 receipt into its own capacity subdirectory before
each job. No native permission case or Engine process is elevated. The exact
header is followed by one blank line and unmodified `LC_ALL=C btrfs filesystem
usage -b WORKDIR` bytes (including its final newline):

```text
schema=loci.btrfs-capacity.v1
source_sha=FINAL_40_LOWERCASE_SHA
run_id=TOKEN
created_unix_seconds=ACTUAL_DATE_SECONDS
workdir=CANONICAL_ABSOLUTE_NATIVE_JOB_CWD
workdir_device=DECIMAL_ST_DEV
workdir_inode=DECIMAL_ST_INO
workdir_mount_id=ACTUAL_HELD_FDINFO_MNT_ID
pilot_entries=100000
pilot_delta_physical=ACTUAL_AFTER_MINUS_BEFORE_2_METADATA_USED_PLUS_DATA_USED
usage_sha256=RAW_PAYLOAD_SHA256

Overall:
... actual raw detailed usage ...
```

The consuming testside helper independently requires root ownership/no writable
group/other bits/no symlink, exact source/cwd/device/inode/mount binding, <=600s
freshness and bounded size/hash, and parses actual DUP/single profiles. Its
resource guard also rechecks current statfs/df15% space; receipt existence alone
cannot turn unavailable inode capacity into a pass. Missing capability/receipt
stays SKIP/UNVERIFIED and is not credited by either runner. The root declaration
proves only preflight, not Engine correctness/full oracle/Task16 completion.
