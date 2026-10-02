# Linux scale checkpoints

Scale Engine save/load now uses LOCISCL1 version 1, while LOCISNP1 remains the
unchanged bounded format. A scale owner rejects old, unknown, incompatible,
damaged or source/scope-mismatched stores with an explicit rebuild error and
preserves their bytes. Rebuilding damaged/incompatible storage is deliberate:
retain or rename the old database and choose a new database destination.

A checkpoint stores the raw canonical root, root device/inode and current mount
identity, scope policy/exclusions, source epoch, immutable publication version,
and every parent/name entry slot with original name bytes, kind, observed object
identity and live/tombstone flag. Observed inode identity is stale across offline
periods. Loaded results remain Pending until watch-before-enumeration correction
finishes. Query pages retain immutable entry-ID ordering.

A successful reopen returns the saved publication version as Pending before any
correction scan runs. The public `poll`/`poll_with_cancel` APIs then drive bounded
correction; `request_rebuild` resumes a cancelled or exhausted attempt. Query leases
on the saved snapshot keep their original paths through correction, while new leases
observe the validated publication after completion. Cancellation, a failed candidate
or changed source identity never relabels the saved snapshot as current.

The length and checksum protect record integrity; independent bounded checks
validate lengths/counts, root/scope, basename bytes, kind/alive flags, parent
references, directory parents, cycles/depth and duplicate live directory entries
before any data is exposed to querying. Input is a regular file opened with
O_NOFOLLOW/O_NONBLOCK under the retained database-parent descriptor. Graph
validation runs in O(number of slots). Memory and encoded byte limits are checked
against the configured scale budgets; corruption never supplies a trusted cursor.

Saving selects the immutable published snapshot exclusively and requires Validated
coverage without gaps. Linux temporary ownership, fsync, renameat and cleanup reuse
the existing parent-fd-bound atomic storage helper. The retained parent identity
and current canonical root containment are rechecked at each save, including
parent pathname redirection or moving that same directory into the indexed root.
A failed write preserves the previous database and cleans owned temporary files.

Each scale owner holds a nonblocking exclusive OS flock on a stable db.lock sidecar
opened with openat/O_NOFOLLOW under the same parent descriptor. Competing writers
fail with WouldBlock. Stop/drop or process termination releases the lock; the
sidecar remains so future owners lock the same inode. Bounded Linux saves respect
the same lock transiently and recheck format/root before saving, preventing a
legacy owner from downgrading scale data created after it opened. External users
must not replace/delete the sidecar while owners are running.

Use the regular CLI with --scale:

~~~sh
cargo run --release -- engine build /owned/root /outside/root/state.loci --scale
cargo run --release -- engine query /owned/root /outside/root/state.loci '' --scale --all --null
cargo run --release -- engine watch /owned/root /outside/root/state.loci --scale --null
~~~

Build/rebuild and complete CLI export drive correction until Validated before
saving/exporting; ordinary first-page queries may report Pending saved results
with exit code 4. Diagnostics remain on stderr, raw NUL-delimited paths on stdout.

Focused development evidence on Linux overlayfs: full public path sets across
save/reopen and independent CLI processes for 5003 real entries containing raw
invalid UTF-8, hard links and symlinks; opt-in release test for 100,000 entries
across 2000 directories; hostile length/checksum/version/graph inputs; writer
contention/release; parent redirection; genuine permission-denied atomic write;
and controlled monitor-process kill followed by restart. The kill test proves
process crash restart, not physical power-loss durability. Ext4/Btrfs and exact
final-commit platform acceptance remain separate gates.
