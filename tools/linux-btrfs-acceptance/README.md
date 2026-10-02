# Checked Btrfs guest handoff

This is a **preparation tool**, not Task 16 acceptance. It checks externally supplied immutable artifacts, launches a bounded QEMU TCG guest without networking, and retains raw evidence. A passing wrapper result means its guest-workload markers passed; `engine_acceptance`, `issue16_resolved` and reference-hardware acceptance remain false. Final native cases, complete oracle, million-entry gates and same-SHA review still require separate assessment.

The existing environment proved guest-kernel Btrfs capability on an older source version. Those results cannot satisfy a final frozen-SHA gate. The old environment builder used private package paths and an old CLI; it is intentionally **not** copied into this repository. No kernel, image, package, binary, private installation tree or previous result is bundled here. This tool neither installs dependencies nor builds/formats/mounts a host disk.

## What the environment must provide

Use Linux with Python pidfd support. Supply an explicitly owned directory containing regular-file QEMU/kernel/initramfs inputs, a read-only artifact image and a distinct writable raw Btrfs data image. Symlink inputs/ancestors, hard-linked writable images, block devices and undeclared existing data images are rejected. Never use a host partition/device or load the guest modules into the host. QEMU uses two vCPUs and 2048 MiB by default; TCG timing cannot satisfy SSD/NVMe reference performance.

The environment must build an initramfs with its matching kernel modules and a guest `/init` that mounts `/dev/vda` read-only as ext4 and `/dev/vdb` as Btrfs, supplies ordinary UID/GID 1000, and runs the final workload as that user. The workload must check actual Btrfs `statfs` magic, `statx` mount identity, actual inotify create/close-write events and attempted writes returning EROFS on the artifact filesystem. Successful shutdown alone is insufficient.

**Guest image assembly, module selection, a final native probe/workload and the guest receipt emitter remain externally supplied and unverified by these source-only tools.** There is no bundled builder or complete Task 16 harness. Existing generic capability markers are deliberately insufficient. A supplied final workload must establish its test/oracle behavior before completion markers are emitted.

## Freeze and preserve artifacts

First freeze the source to a complete lowercase 40-hex SHA and prepare compiler receipts with the [shared Linux artifact runner](../linux-native-acceptance/README.md). That runner records clean source/tree identity, compiler/profile/commands, binary digests and build logs. Use its existing `stage_tools` flow; do not create a second runtime-dependency resolver.

The shared runner stages GNU `sort`, `sha256sum` remains an explicit guest requirement, and BusyBox `sort` does not meet the controller's `sort -z -S64M` contract. Include the shared runtime overlay and required guest libraries at their canonical absolute paths. Stage a compatible `sha256sum` through environment preparation if the guest lacks it. Copy the CLI, test binaries and driver into the artifact image while preserving **all manifest absolute destinations**. Tests bake `CARGO_BIN_EXE_loci-experiment` paths during compilation; those exact paths must resolve inside the guest. For example, a guest `/workspace` symlink can point into `/artifacts/workspace`, but additional absolute prefixes must be represented deliberately. The driver itself does not bake that CLI path.

The final million driver is `linux_million_acceptance --controller` with `--phase all --repetitions 200 --idle-seconds 600`. Queries are UTF-8 plain lines, including a meaningful empty first line. Use the [repository suite](../linux-million-acceptance/queries.txt) and its exact digest, not JSON or a reduced baseline. The current foundation
suite has 52 lines (including the empty first line), SHA256
`53b6379f77d0fb79c134285b92382347a2c029d598f5278ad2d011eae4b99a74`;
freeze/recheck it with the final source revision. Coordinate the final driver and [soak tool](../linux-soak/README.md) receipts; this wrapper neither runs nor resolves the 24-hour gate. Its summary does not
set `native_behavior:true` and cannot be used directly as a passed Task 16
prerequisite for the soak controller. Only reviewed complete native evidence can
support that separate receipt.

Create an external JSON content manifest with `source_sha` and `artifacts`, each with a unique absolute `guest_path` and `sha256`. Include every compiled test, CLI, driver and shared runtime overlay entry at its original absolute destination, plus the guest probe/workload. The wrapper verifies the host build artifacts and mapping. The guest must independently hash its mounted content manifest and **all entries on the actual artifact filesystem** before printing the source/content receipt. A source SHA printed from kernel arguments alone is not sufficient. The outer image digest pins that content manifest and workload in the read-only image.

## Ownership and budgets

Create a separate data-image ownership receipt at provisioning time. It must contain `schema: "loci.btrfs-owned-data.v1"`, the exact resolved `path`, `uid`, `device` (`st_dev`), `inode` (`st_ino`) and the specific authorized `run_id`. The outer manifest pins this receipt's SHA256. No existing image is accepted merely because its filename looks task-owned. Each run uses a unique token and a fresh results directory; results are never overwritten.

Budget Btrfs data separately from logical metadata. Metadata profile `DUP` consumes **two physical copies**. The planned `data_bytes + 2 * metadata_logical_bytes + sort_and_logs_bytes` must leave at least 15% of the guest image's logical size unbudgeted. Host preflight separately reserves the entire sparse data image's possible growth plus the serial-log budget and keeps at least 15% of the host backing filesystem free. This is conservative planning, not proof of actual Btrfs metadata capacity. Record actual `btrfs filesystem usage -b`, profiles, allocation and a representative capacity pilot in final workload evidence. Unsupported/unverified pilots leave the gate open.

Btrfs commonly reports `statvfs.f_files=0`; the checked output records the inode metric as **unavailable**, with raw measurements and `metadata_capacity_verified:false`. It substitutes no ext4 inode estimate and does not call zero an inode-exhaustion failure. Native driver capacity assessment remains separate.

## Input manifest and invocation

The outer JSON manifest uses this structure (replace every placeholder with real receipts/digests; do not fabricate build provenance):

```json
{
  "schema": "loci.btrfs-guest-input.v1",
  "source": {"sha": "EXACT_40_LOWERCASE_HEX"},
  "sha256": {"qemu": "DIGEST", "kernel": "DIGEST", "initramfs": "DIGEST", "artifact_image": "DIGEST"},
  "build_manifest": {"path": "/owned/build/manifest.json", "sha256": "DIGEST"},
  "guest_content_manifest": "/owned/guest/content.json",
  "guest_content_manifest_sha256": "DIGEST",
  "data_image_owner_receipt": {"path": "/owned/guest/data-owner.json", "sha256": "DIGEST"},
  "btrfs_budget": {
    "metadata_profile": "DUP", "metadata_copies": 2,
    "data_bytes": 2147483648, "metadata_logical_bytes": 1073741824,
    "sort_and_logs_bytes": 2147483648
  }
}
```

The example budget does not promise capacity for a particular corpus; select/provision a sufficiently large owned image and measure the final workload. Provide firmware/data files and a private QEMU library directory explicitly when required:

```sh
python3 tools/linux-btrfs-acceptance/runner.py run \
  --owned-prefix /owned/guest --source /owned/frozen-source --sha EXACT_40_SHA \
  --qemu /owned/guest/bin/qemu-system-x86_64 \
  --kernel /owned/guest/kernel --initramfs /owned/guest/initramfs.cpio.gz \
  --artifact-image /owned/guest/artifacts.ext4 --data-image /owned/guest/data.btrfs \
  --artifact-manifest /owned/guest/guest-manifest.json \
  --qemu-data-dir /owned/guest/firmware --bios /owned/guest/firmware/bios.bin \
  --qemu-library-dir /owned/guest/lib \
  --results /owned/guest/results-UNIQUE_ID --run-id UNIQUE_ID \
  --host-write-budget-bytes PLANNED_BYTES --timeout 3600 --plan-only
```

`--plan-only` verifies supplied inputs and writes the exact command without starting QEMU. Remove it only for an authorized measurement window. Avoid QEMU during other owner CPU/latency/600-second quiet measurements. Firmware/library provenance must accompany the environment receipt; these directory contents are not recursively attested by this thin launcher. QEMU `-nic none`, artifact `readonly=on`, explicit input hashes and post-run source/image identity checks are recorded. The only writable guest disk is the declared data file; the results path is outside the guest artifact image.

## Required serial protocol and timeout behavior

A successful final guest emits these exact standalone lines, after the checks they describe:

```text
LOCI_BTRFS_SOURCE sha=<SHA> content_sha256=<CONTENT_DIGEST> run_id=<TOKEN>
LOCI_BTRFS_ARTIFACT_READONLY verified=1
NATIVE_CAPABILITY uid=1000 gid=1000 statfs=9123683e mount_id=<N> inode=<N> inotify_bytes=<N> create=1 close_write=1
NATIVE_STATVFS files=<N> ffree=<N> favail=<N> blocks=<N> bfree=<N> bavail=<N> frsize=<N>
LOCI_BTRFS_WORKLOAD_PASS run_id=<TOKEN>
LOCI_BTRFS_COMPLETE run_id=<TOKEN>
```

The native probe's mount ID, inode and inotify byte count must be positive. Kernel/guest shutdown noise, source/content/token mismatch, absent proof, nonzero QEMU exit, `SKIP:`, `UNVERIFIED:`, `GUEST_CAPABILITY_FAIL` or `LOCI_BTRFS_FAIL` prevents a passing wrapper result. Preserve concrete test names/reasons in raw guest logs; the wrapper's marker check is not a full libtest result classifier. Use the shared Linux classifier for each native test log, with its real exit status. Windows or FFI type checks are separate evidence and never count as Btrfs execution.

Timeout and log-budget termination signal only the held **pidfd of the directly spawned QEMU process**. It never reacquires a reused PID or kills a process group. TERM is followed by bounded waits and KILL if needed. A failed kill/wait records root release as unconfirmed; descendants are always unconfirmed. Do not reuse images or delete evidence while any process remains active. Serial output has a configurable byte budget; exceeding it fails rather than truncating a passing result. Raw `serial.log`, `plan.json` and `summary.json` remain in the fresh results directory. A completed wrapper cannot authorize resolving Task 16.

## Lightweight verification

```sh
python3 -m py_compile tools/linux-btrfs-acceptance/runner.py
python3 -m unittest discover -s tools/linux-btrfs-acceptance -p 'test_*.py'
```

Tests use small synthetic logs and a sleeping stand-in child alongside an unrelated process to check marker/receipt validation, unavailable inode reporting, writable-image rejection and held-pidfd timeout behavior. They never execute QEMU, build Rust/images, mount Btrfs or produce native acceptance evidence.
