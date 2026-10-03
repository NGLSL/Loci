# Exact-SHA native ext4 execution tools

This development tool prepares immutable build artifacts and runs the public
Engine/CLI tests in an environment supplied by the operator. It is an execution
entry for Linux filesystem acceptance, not an acceptance result or a ticket
resolver. No build or native workload starts until an explicit `prepare` or
`run` command is invoked. Lightweight checks never compile Rust, build an image,
start a VM, or create a large fixture.

## Required environment

Provide a Linux Rust toolchain, Docker, an owned artifact/results location and a
trusted native-test image. The repository does not bundle that image or its
launcher. `--image` and `--image-id` are required and must identify the image
created by environment preparation. `--container-launcher` selects the image's
launcher, conventionally `/usr/bin/loci-run-ext4`. Its actual digest is retained
in native results.

The supplied launcher must create an isolated ext4 volume with sufficient space
and inodes, mount it at `/mnt/loci-native`, and execute the remaining arguments
as UID/GID 1000. The existing environment preparation uses a fresh 8-GiB image
with 1.5M inodes, unoccupied loop assignment, and an unmount trap. This is a
launcher contract, not a script distributed here. The runner requires actual
ext4/UID/GID checks to pass before any native workload. It does not mount host
partitions, request a privileged container, or change sysctl limits. The launch
uses network none, SYS_ADMIN, and loop-device rules; private bind-mount tests
create their own user/mount namespace.

The base image must provide Bash, `cat`, `cp`, `df`, `findmnt`, `id`, `mkdir`,
`mount`, `stat`, `timeout`, `umount`, `uname`, `unshare`, `sha256sum`, the launcher,
and their runtime libraries. The [archived environment source](environment/README.md)
provides a parameterized launcher and explicit rootfs staging glue; it does not
bundle a built image or claim final native acceptance. `prepare` snapshots missing helpers `kill`, `mkfifo`,
`true`, GNU `sort` and their `ldd` libraries into the owned artifact directory. It never
mutates a shared rootfs, image or global installation. `Dockerfile.runtime`
allows a later unified runtime-image build; by default the runner mounts those
snapshots read-only at their exact tool/library paths with digest verification.
The million controller requires GNU `sort -z -S` for bounded raw-path sorting;
minimal images and BusyBox support are not assumed.
Runtime snapshots, images, result logs and preparation artifacts belong outside
Git.

Do not compile, build images, run full tests, QEMU, or scale fixtures during an
active quiet/idle measurement. Btrfs, native Windows and other final gates require
their own same-SHA environments and evidence; this ext4 runner does not infer
those results.

## Build and provenance

`prepare` requires a clean tracked source tree at the exact requested 40-hex HEAD.
It compiles debug/release unit and integration test executables through Cargo
JSON artifacts and records compiler/profile data, Rust/Cargo versions, complete
commands, absolute artifact paths, sizes and SHA-256 digests. It checks source
identity before and after builds. A `--rust-env` shell file is optional; otherwise
it uses the configured PATH/toolchain. `--env-file` remains an alias. Source,
artifact and Rust environment paths are operator parameters.

`run` verifies the exact clean SHA and every artifact/runtime digest. Source and
complete target directory are read-only container bindings at the same absolute
paths, preserving the tests' baked `CARGO_BIN_EXE_loci-experiment`. Source/artifact
identity is checked again after execution. Results record actual mount flags,
kernel, CPU, memory, cgroup CPU/memory and inotify limits, disk/inodes, UID/GID,
image ID, launcher digest, exact commands and raw outputs.

Each lib/unit/integration binary runs once per profile, serially, with
`--test-threads=1 --nocapture` and an explicit process/group timeout. Dedicated
100k and actual-overflow cases then run as separate named phases. Every process
has its own raw stdout/stderr log and exit/elapsed record.

Compiled artifacts exclude Rustdoc execution. If tracked Rust source contains
Rustdoc code fences, the manifest lists them and execution remains unverified
until separate same-SHA doctest evidence is supplied. Compiled suites are never
silently reported as a Rustdoc result.

## Commands

Choose fresh owned artifact and result paths: existing directories are refused,
so previous raw evidence is never overwritten. Substitute an actual full reviewed
SHA, source/environment paths and the prepared image identity below.

```sh
python3 tools/linux-native-acceptance/runner.py prepare \
  --source /path/to/checkout --sha FROZEN_SHA \
  --artifacts /owned/artifacts/FROZEN_SHA \
  --rust-env /path/to/rust-env.sh \
  --driver-example linux_million_acceptance

# Review generated commands without starting a container.
python3 tools/linux-native-acceptance/runner.py run \
  --manifest /owned/artifacts/FROZEN_SHA/manifest.json \
  --results /owned/results/ext4-plan-FROZEN_SHA \
  --image PREPARED_IMAGE --image-id sha256:VERIFIED_IMAGE_ID --plan-only

# Native compiled suites plus explicit 100k/overflow, debug and release.
python3 tools/linux-native-acceptance/runner.py run \
  --manifest /owned/artifacts/FROZEN_SHA/manifest.json \
  --results /owned/results/ext4-native-FROZEN_SHA \
  --image PREPARED_IMAGE --image-id sha256:VERIFIED_IMAGE_ID
```

`--driver-example` is optional when preparing compiled suites only. If requested,
the real example must exist and build successfully; no placeholder is executed.
The default phase list is `suites,100k,overflow`. The dedicated 100k cases cover
100,000 real entries across 2,000 directories, local updates/full oracle, and
checkpoint/reopen/CLI export. Their in-process oracle memory is harness memory,
not isolated engine RSS. Dedicated overflow runs the public scale Engine's actual
IN_Q_OVERFLOW assertion and both ignored bounded compatibility fixtures. The
older fixtures may print SKIP for their conservative generation budget; those
specific gates remain unverified even if the scale overflow proof passes.

Both 100k fixtures share `tests/common/fixture_capacity.rs`. Reported filesystems
retain the greater-than-800,000 KiB, greater-than-125,000 inode and 15% free-space
guards. A confirmed Btrfs zero inode counter is labelled unavailable and still
requires actual `btrfs filesystem usage -b` with Metadata,DUP and Data,single,
minimum free/unallocated physical capacity, and a positive measured pilot slope.
The plan reserves 1.5 times that slope for 125,000 entries plus 512 MiB for
fixture data, checkpoint and logs, leaving at least 15% of the physical device.
Direct query access creates only an owned 1,000-entry pilot outside the searched
root and removes it after measuring; it does not create the full fixture early.

When UID1000 cannot query detailed Btrfs capacity, the root environment may
provide `LOCI_BTRFS_CAPACITY_RECEIPT` and `LOCI_TEST_SOURCE_SHA`. The regular,
root-owned receipt must be inaccessible to group/world writes, fresh within
600 seconds, and bind the canonical working directory, inode, device, mount ID,
frozen source SHA, actual pilot entries/physical delta and hashed raw usage.
Its schema is `loci.btrfs-capacity.v1`: key=value headers, a blank line, then
the unchanged native usage output. This is capacity input only; the tests still
create all 100,000 entries and validate the complete native oracle themselves.
Missing/unsupported capacity evidence prints `SKIP: UNVERIFIED:` and returns;
the libtest pass must not earn native acceptance credit. Insufficient measured
capacity fails the test. No helper installs tools or mounts a filesystem.

## Interpreting results

`summary.json` classifies every job as passed, failed or unverified. `SKIP:` and
`UNVERIFIED:` messages retain concrete test name, reason and raw line, even with
libtest exit 0. Such tests are removed from credited passes; raw libtest totals
remain explicitly labelled non-acceptance counts. Unattributed skips produce no
credited total. Ignored test names and reasons remain separate and earn no credit.
A panic, nonzero exit, timeout, missing completion summary, missing exact test or
missing positive actual-overflow marker is a failure. A namespace/overflow skip
keeps that requirement open regardless of the rest of the suite's pass total.

Exit 0 means the selected execution phases completed without reported failure or
environment skip. Exit 4 means unverified coverage, exit 1 means test/execution
failure, and exit 2 means input/preparation/provenance failure. All raw evidence
survives. `full_issue15_acceptance_claimed` is always false: the operator must
assess the same-SHA filesystem/1M evidence and remaining specification gates.

A retained log can be assessed without executing a workload:

```sh
python3 tools/linux-native-acceptance/runner.py classify-log \
  --log /owned/results/native-case.log --exit-status 0 \
  --required-one-test --required-marker native_scale_kernel_overflow_verified
```

Cloud SSD/NVMe backing and reference hardware are never inferred from timing.
The runner records `reference_hardware_verified=false`; reference acceptance
needs independently verified hardware. fsync/rename and process crash tests
establish only the tested process/crash scope, not physical power-loss durability.
No 24-hour, million-entry, Btrfs or native Windows result follows from this tool's
existence or a smaller fixture's success.

## Million-driver hook

The optional hook uses the separate `linux_million_acceptance` example. It does
not duplicate the production worker, fixture/oracle implementation or query
benchmarks. `--queries` takes UTF-8 plain text with one literal query per line,
not a JSON array. Use the final driver's complete query suite rather than a
smaller baseline; retain its digest with the driver results. Check that example's
final interface before native execution. The current [repository suite](../linux-million-acceptance/queries.txt)
contains 52 lines, including an empty first query; preserve that line.

```sh
python3 tools/linux-native-acceptance/runner.py run \
  --manifest /owned/artifacts/FROZEN_SHA/manifest.json \
  --results /owned/results/ext4-all-FROZEN_SHA \
  --image PREPARED_IMAGE --image-id sha256:VERIFIED_IMAGE_ID \
  --phases suites,100k,overflow,million \
  --fixture-data /owned/fixture/data --queries /owned/fixture/queries.txt
```

The source corpus binds read-only at `/input` and copies as real files onto the
fresh owned ext4 root. Copying requires at least 1.15M free inodes, 1.5GiB free
space and 15% reserve. The controller receives `--controller --root ABS --database
ABS --output ABS --sha 40HEX --queries FILE --phase all --repetitions 200
--idle-seconds 600`. The database is outside selected data but remains on ext4;
results bind to durable output and the final database is copied into the archive.
The controller must generate an oracle with the actual ext4 absolute prefix.
Missing driver artifacts fail. Driver exit 0 is execution evidence only; its
JSONL/manifest must establish actual 1M requirement results.

## Lightweight checks

```sh
python3 -m py_compile tools/linux-native-acceptance/runner.py
python3 -m unittest discover -s tools/linux-native-acceptance -p 'test_*.py'
```

These checks use the public log-classification/preparation CLI and small retained
log examples. They do not invoke Cargo, Docker, native test binaries or fixtures.
