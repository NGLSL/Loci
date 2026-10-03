# Disposable ext4 environment source

These source files archive the task-specific scratch-image glue formerly kept only in an external prepared environment. They do not establish Task 15 native acceptance. No kernel, package, binary, Docker image, rootfs snapshot or prior result is committed here. No downloads, host installation, sysctl writes or host partition formatting are performed by these sources.

`Dockerfile` builds an image from an **externally supplied, explicit dependency snapshot**. `stage-rootfs.py` copies that snapshot into a fresh task-owned build context, pins source glue and every resulting file by SHA256, and supplies the archived launcher and UID1000 passwd/group templates. It never builds or runs Docker. Input libraries, dynamic loader aliases, GNU/Bash utilities and `mke2fs.conf` must already be available from environment preparation; this is not a package installer or a second dependency resolver. The original ext4 environment had no separate C native probe source; actual native event, permission, overflow and release checks remain in the repository's public Rust tests. The separate Btrfs probe belongs to its [Btrfs environment](../../linux-btrfs-acceptance/README.md).

## Stage an owned context

Provide `--input-root` and an external JSON dependency receipt:

```json
{
  "schema": "loci.ext4-environment-input.v1",
  "files": [
    {"source": "usr/bin/bash", "destination": "/usr/bin/bash", "sha256": "ACTUAL_64_HEX_DIGEST"}
  ]
}
```

This abbreviated example is not a complete runnable rootfs. Include the full tool/config set required by `stage-rootfs.py`, every dynamic dependency and its canonical loader/library aliases. Source paths must resolve inside the declared input root; destinations must be unique absolute paths without traversal. Existing output contexts are rejected. Preserve executable bits. The archived launcher/passwd/group destinations cannot be overwritten by dependency entries.

```sh
python3 tools/linux-native-acceptance/environment/stage-rootfs.py \
  --input-root /owned/prepared-dependencies \
  --input-manifest /owned/dependency-receipt.json \
  --output-owned /owned/fresh-ext4-context
```

Only after source review and an authorized build window, build the supplied context with Docker and record the resulting exact image ID and `environment-manifest.json`. Use that image with the [native artifact runner](../README.md), which verifies the explicit image/launcher identity and preserves compile-time absolute artifact paths. The shared artifact runner's `stage_tools` supplies missing helpers and **GNU sort**, together with its libraries. GNU `sort -z -S64M` and `sha256sum` must resolve at their expected paths; BusyBox sort is insufficient. Do not copy private package directories or a host rootfs into Git.

## Launcher contract

The image supplies `/usr/bin/loci-run-ext4`. Existing `loci-run-ext4 COMMAND ARG...` callers retain default `/mnt/loci-native`, a fresh 8 GiB sparse image, 1,500,000 requested inodes and a 4 GiB extra backing-filesystem write budget. Parameters allow explicit task-owned locations and budgets:

```sh
/usr/bin/loci-run-ext4 \
  --owned-prefix /tmp/ext4-UNIQUE_RUN --mountpoint /mnt/loci-native \
  --image-bytes 8589934592 --inodes 1500000 \
  --host-extra-write-bytes 4294967296 -- /usr/bin/bash /owned/native-plan.sh
```

The owned prefix and mountpoint must not exist, must have existing direct parents without symlinks/dot components, and must be separate absolute paths outside `/dev`, `/proc` and `/sys`. No caller-supplied image/device is formatted: the launcher derives `data.ext4` inside its new mode-0700 prefix. It checks that full sparse-image growth plus declared additional writes leaves at least 15% of the backing filesystem free. That planning reserve does not replace workload-specific free-byte/inode/resource checks. Choose budgets from the actual full fixture/oracle/output plan; no reduced fixture becomes acceptance.

`--print-plan` performs argument checks and prints a quoted command without creating directories, images, devices or mounts. Real setup requires container root, `LOCI_NATIVE_CONTAINER=1` and a Docker/Podman container marker. The supplied Dockerfile sets that variable. These are guardrails for a disposable container, not an isolation proof against a deliberately malicious privileged caller. The external container must have only the loop/mount capabilities/device rules needed by the native runner, disabled networking, read-only artifact binds and fresh owned results. Never launch this source on the host or give it host block devices.

Loop allocation uses `mount -o loop` and kernel autoclear. The launcher creates container-local loop device nodes but never chooses, formats or detaches an occupied numbered loop. It records ext4 mount options, changes only the new mounted root's owner to UID/GID1000 and invokes the workload through `setpriv --clear-groups`. Permission tests therefore run as an ordinary user.

EXIT/INT/TERM traps unmount the owned mount and query loop associations for the exact new image. Cleanup success emits `LOCI_NATIVE_ENV_CLEANUP_PASS mount_released=1 loop_released=1`. Failed unmount/association checks or leftover owned paths emit `LOCI_NATIVE_ENV_CLEANUP_UNVERIFIED`, fail a previously successful workload and preserve unresolved owned resources. No lazy unmount or unrelated loop detachment conceals failures. Abrupt SIGKILL, blocked kernel I/O or container shutdown may prevent the trap; absent cleanup evidence cannot prove release. The outer runner must retain timeout/raw logs and review actual cleanup. Workload exit status is preserved when cleanup succeeds.

The parameterized/guarded source version has only lightweight checks here; its final image/runtime behavior must be rerun at the frozen source SHA. Earlier environment capability or a successful Docker build cannot close Task 15, satisfy reference-hardware performance or prove physical power-loss durability.

## Lightweight checks

```sh
bash -n tools/linux-native-acceptance/environment/run-ext4.sh
python3 -m py_compile tools/linux-native-acceptance/environment/stage-rootfs.py
python3 -m unittest discover -s tools/linux-native-acceptance/environment -p 'test_*.py'
```

These checks use print-plan and tiny invalid dependency receipts; they do not mount, format, build images, compile Rust or run native acceptance tests.
