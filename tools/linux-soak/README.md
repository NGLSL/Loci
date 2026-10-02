# Continuous native Linux 24-hour supervisor

This controller is the reusable execution entry for LOCI-LINUX-001-17. Creating
it, running its unit tests, or passing its small protocol smoke does **not** pass
the issue. The official `run` requires the reviewed frozen artifact and completed
same-SHA ext4/Btrfs behavior gates 15/16. It measures one uninterrupted supervisor
window of at least 86,400 real monotonic seconds and records UTC timestamps.
There is no shortened acceptance mode or virtual clock.

It drives the production public Engine/monitor through the issue14 worker;
it does not duplicate a Rust driver or access private engine state. Prepare
commands never start an engine, VM, fixture generator, compilation or workload.
A real 24-hour run has not been performed by this implementation commit.

## Runtime and provenance

Use Linux, Python3 with `os.pidfd_open`/`signal.pidfd_send_signal`, GNU `sort` with
`-z -S -T`, `/proc` including readable PID fdinfo, and the frozen release
`examples/linux_million_acceptance` binary. Execute the supervisor and worker as
UID1000 against a dedicated, owned, real million-entry ext4 fixture. Its
`mutation_parent` is an existing ordinary directory which this workload may
rename, chmod, and populate with temporary files. Database, output and logs must
stay outside the indexed root. Neither symlinks nor nested mounts are followed;
mount boundary entries remain searchable. Raw filename bytes, hardlink paths and
multiplicity are preserved in the independent raw-NUL oracle.

In a container, bind the full evidence run to an **owned persistent host**
directory before launch. A `docker --rm` writable layer is insufficient. Preserve
the original host directory, build manifest, image identity, fixture marker and
raw results. Stage Python/stdlib and GNU sort plus their loader/library digests
into the runtime; the ext4 native-test image does not imply Python is installed.
The native execution runner in `../linux-native-acceptance/` supplies artifact
preparation conventions, not a 24-hour container launcher. No network is needed
while running. Do not run QEMU, compilation or competing benchmarks during the
hourly quiet measurement. Do not change host modules, sysctl or global limits.

Copy `config.example.json` to the owned evidence location and replace all paths,
source SHA, binary SHA256 and fixture ownership values. Zero identities work for
`prepare` only. The template's root/marker names are examples, not an existing
fixture promise. Bindings must retain the absolute CLI/artifact paths from the
build manifest. Source identity supplied to HELLO is not proof of compilation:
the controller checks the actual `/proc/PID/exe` SHA256 and start ticks and uses
pidfds for signals. Each worker segment records its identity and startup timing.

The normalized build receipt consumed by `build_manifest` has:

```json
{"source_sha":"FULL_FROZEN_SHA","tracked_clean":true,"worker_binary_sha256":"ACTUAL_SHA256"}
```

Generate that receipt from the retained clean exact-SHA build/artifact manifest;
retain and link the original manifest alongside it. Both prerequisite receipts
have `status: "passed"`, `source_sha` equal to the frozen SHA and
`native_behavior: true`; attach the original full filesystem/scale evidence.
A skipped, partial or exploratory run must never be normalized to a pass.
`persistent_evidence_attestation` records the actual inspected bind:

```json
{"persistent_host_owned":true,"host_path":"/actual/host/evidence/run","controller_path":"/owned/host-backed/soak-run"}
```

All receipts are operator/orchestrator evidence inputs; review their backing
before starting the final gate. Changed production behavior needs a fresh frozen
build and affected gates. Cloud overlayfs, Btrfs TCG guest functionality and
unknown SSD/NVMe backing are not reference-hardware performance evidence. Windows
native behavior, kernel slab bytes and physical power-loss durability remain
separate unverified requirements.

## Commands

From the source checkout, with a real configured persistent run directory:

```sh
python3 tools/linux-soak/soak_controller.py prepare \
  --config /owned/config/soak.json --run-dir /owned/host-backed/soak-run

# This starts the actual million-entry production owner and a true 24-hour window.
python3 tools/linux-soak/soak_controller.py run \
  --config /owned/config/soak.json --run-dir /owned/host-backed/soak-run
```

Run under a continuous host service/session with wall time available for the
whole window plus startup/final verification. The controller refuses another
live supervisor with the same PID/start ticks/boot ID. SIGINT/SIGTERM records an
interruption, cancels jobs, joins the owner and verifies resource release where
possible. An explicit operator interruption may instead use a bounded
`interrupt.json` inside the run directory, containing `{"reason":"actual reason"}`.
Remove that request only after inspecting the recorded interruption.

```sh
# After a dead/interrupted supervisor, this starts an entirely new eligible day.
python3 tools/linux-soak/soak_controller.py run \
  --config /owned/config/soak.json --run-dir /owned/host-backed/soak-run --resume
```

`--resume` preserves manifests/logs and safely restores recorded owned mutations;
it **never adds earlier elapsed segments** toward 86,400s. An ambiguous mutation
ownership receipt blocks cleanup rather than deleting an unproven object. A
changed SHA/binary or a protocol-smoke run requires a new run directory.
Interrupted raw sets continue counting against the total budget; inspect/archive
them outside the new owned run when its remaining budget cannot admit a day.

For development only, point a separate config/run at a tiny UID1000 fixture
(`baseline_entries` <=1000) and use `protocol-smoke`. It exercises real independent
full sets, held leases, file add, directory rename, permission gap/rebuild,
checkpoint and same-live-PID release without creating an eligible day window.
Its manifest always says `engine_acceptance: false`. The test double bundled here
only validates transport/error handling and never claims native behavior.

## Hourly schedule and stable results

| Minute | Required actual workload |
| --- | --- |
| 0–10 | One uninterrupted 600s quiet CPU/RSS window |
| 10–30 | Slow pinned reader plus bounded 2 ops/s add/rename/delete and directory rename |
| 30–35 | Complete independent raw-byte oracle, save and stable tail |
| 35–40 | Actual kernel/user queue storm or public rebuild cancellation; full recovery oracle |
| 40–45 | UID1000 chmod000 exact Permission/errno13 gap, restore/rebuild, full oracle |
| 45–50 | Selected graceful/crash restart with offline mutation, searchable Pending checkpoint and full correction oracle |
| 50–60 | Save, final stable full set, resource point and stable tail |

Actual kernel overflow runs hours0/12; user-queue loss hours6/18. Other hours
exercise public cancellation. Graceful restarts are hours5/17 and process crash
is hour11. Storms only SIGSTOP the bound owned worker, create actual alternating
filesystem events, SIGCONT it, and require public `KernelOverflow`/`UserOverflow`.
No injection opcode or shortened proxy proves loss. A host inotify queue beyond
the configured event budget fails preflight for that phase without sysctl changes.

A stable cut requires two spaced gap-free Validated observations, a complete
export whose every page stayed validated, the same version before/after, and an
exact independent sorted-NUL full comparison including count/multiplicity. A
held slow lease is compared against its earlier independent full oracle; newer
validation is not attributed to that old snapshot. Failures keep complete raw
sets and stop; no sampling or first50 substitutes for an oracle. Export operations
are explicitly OP_DROP'd after terminal results to release bounded job state.
All restore/correction phases record failures and status rather than treating
Pending/Failed/Stopped as corrected.

The external sampler continues through oracle walking/sorting and records actual
worker RSS/HWM/CPU/fd/kernel-watch counts every5s. Stale cached public observations
are labelled with their age. Each quiet window requires CPU <=1% of one core and
end RSS <=200MiB; process HWM must stay <=512MiB. Public configured queue/snapshot/
retained limits must exist and remain bounded. Fixed-topology stable points check
watch/fd accumulation and persistent same-process RSS growth, preserving curves
for review. Kernel slab bytes are explicitly unavailable rather than inferred.
After public STOP joins/drops jobs, leases and owner, the worker remains alive:
both worker and independent `/proc/PID` samples must equal pre-open fd/watch
baseline. Only then is QUIT sent. Merely exiting the process cannot prove release.

## Disk, logs and failure retention

The default **4GiB aggregate includes all retained windows, logs, raw exports,
hex-path kind sidecars, independent oracles, sorted duplicates, GNU sort spills,
checkpoint and manifests**. It is not a per-file budget. Logs additionally total
<=256MiB across resumes, split by hour/8MiB files; event/resource/stderr class
caps remain bounded. Each raw result or kind file is <=512MiB. A kind sidecar
`hex(path) TAB F/D/L` can be roughly twice path bytes: the real corpus's ~87MB raw
set implies ~173MB kinds. Admission reserves up to three raw-file budgets for
oracle work, five for export/sort/kinds, and one for save before writing; actual
aggregate usage is checked throughout asynchronous work and every5s. Production
worker receives `--output-budget-mib 4096` and also enforces its own write budget.

Both fixture/evidence filesystems must retain 15% free blocks and >=64MiB after
admission. Fixed-inode filesystems retain >=2048 inodes. This tool's official gate
requires ext4; Btrfs dynamic inode counters and metadata DUP need the separate
filesystem-aware fixture preflight. The host backing a sparse filesystem image
needs its own reviewed free-space guard and enough worst-case allocation. The
controller's inner statvfs is not evidence of host sparse-image headroom.

Successful comparisons retain SHA/count/version receipts and remove only owned
redundant raw/kind/sorted/oracle files. The independent old oracle is kept for the
slow-reader interval, then removed after success. Failure keeps raw/partial sets
and bounded logs for inspection and stops; it never deletes failure evidence to
claim a pass. Reserve enough persistent disk before launch, rather than increasing
a file limit blindly during a run. Do not put evidence inside the indexed root.

## Lightweight checks

```sh
python3 -m py_compile tools/linux-soak/soak_controller.py
python3 -m unittest discover -s tools/linux-soak -p 'test_*.py'
```

These tests use small real files and a visibly labelled protocol double. They
verify framing, wrong/oversized replies, bound process identity, raw oracle,
create-new output, aggregate admission, hourly log rotation, resume classes and
non-accumulation. They do not compile Rust or start Docker/QEMU/million fixtures.
Inspect `run.json`, each window's logs, retained failure files and original gate
receipts before crediting issue17; `passed` is possible only after the actual day,
all hourly schedules/oracles and independent graceful-release proof.
