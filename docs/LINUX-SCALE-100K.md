# Real 100k Linux development milestone

Linux scale mode now defaults to one million live entries, 32,768 directories
and 4,096 entries per scan batch. These are configured capacities, not proof of
the million-entry acceptance or its performance targets. The bounded default
Engine and old format remain unchanged.

The opt-in real-filesystem test creates exactly 100,000 entries: 2,000
directories with 49 real files each. It checks inode/disk reserves first, then
compares complete snapshot-bound pages with an independent filesystem traversal.
It repeats full-set checks after native file add, rename and delete, directory
rename, later child creation and subtree deletion. A held old lease retains its
earlier paths. Local-work metrics verify these reliable changes do not rescan the
root or clone the complete inventory. Ordinary tests cover budget failure,
reader release and CLI batch completion without creating the large fixture.

Reproduce the large development run with Rust 1.99:

```sh
cargo test --release --offline --locked --test linux_scale_100k -- --ignored --nocapture --test-threads=1
```

On the development Linux 6.18.44 x86_64 overlayfs environment (4-CPU/16-GiB
cgroup), the initial debug run built the real inventory in 0.472 s and enumerated
all 100,000 paths in 0.226 s. It held 2,001 actual native watches. The three
isolated file operations became query-visible in roughly 3–6 ms; each touched
one entry and copied one segment containing at most 674 records. These are
single-run baseline observations, not p95 product acceptance. The test process,
including fixture/oracle/path vectors, reported 46 MiB RSS and 63 MiB HWM; this
is not isolated engine-process RSS. Kernel watch bytes were not measured.

The focused release run also passed the complete oracle and live-update matrix,
including local subtree deletion. It built in 0.297 s, enumerated all rows in
0.160 s and observed the three isolated file changes in 1.8–4.0 ms. Its harness
RSS/HWM was 45/62 MiB. The same limits and measured one-segment local work
applied; these remain overlayfs development baselines.

Scale CLI examples (database outside the selected root):

```sh
cargo run --release -- engine query /chosen/root /outside/scale.loci '' --scale --all --null --page-size 1024
cargo run --release -- engine watch /chosen/root /outside/scale.loci --scale --null
```

All commands wait for the first published snapshot while driving the same
bounded-batch Engine. `--entries`, `--directories` and `--scan-batch` explicitly
configure scale limits. Watch commands remain `query`, `export`, `status`,
`rebuild`, `save`, `stop`. Scale build/save and save-on-stop currently report
Unsupported; no old-format checkpoint is written. Scale persistence is the next
dependent milestone.

`EngineOptions.scale_budgets` independently bounds physical slots (including
deleted slots and root), original-name arena bytes (including obsolete rename
names), single snapshot vector capacity, retained shared segment capacity,
application event-queue bytes and query leases. Inventory allocation and segment
copy require available retained-byte credit before mutation; snapshots deduplicate
shared segments when checking retained capacity and publication. Old published
queries survive failed allocation or incomplete correction. Watch/source queues
retain their separate fixed event/buffer limits; resource status reports both
source and application queue totals. Directory adjacency and child positions
remain owned by the writer, allowing local unlink and subtree removal without
scanning every inventory slot. No full writer lookup is copied into snapshots.

These capacity accounts describe owned Rust vectors and segment payloads, not
process RSS, allocator metadata or kernel memory. Writer lookup/adjacency is
bounded by live entry and physical slot limits. Memory credits are currently
per Engine owner; only native watch/fd pools are process-wide. Shared memory
accounting and compaction remain subsequent resource-scheduling work. Exhausted
physical slots or obsolete arena bytes cause explicit failure/correction until
compaction is available; they never make old results falsely current.

This development evidence does not establish million-entry targets, ext4/Btrfs
acceptance, native Windows behavior or 24-hour stability. Those gates execute
separately on the final integrated production commit.
