# Linux scale entry inventory behind Engine

Status: accepted for the small native model milestone (LOCI-LINUX-001-04).

The bounded Engine remains the default on Linux and Windows, including its
limits, query ordering and LOCISNP1 database format. Linux callers opt into a
private scale runtime using `Engine::open_with_options` and
`EngineOptions::scale()`. Query handles, leases, search and snapshot-bound pages
retain one public Engine facade. Windows rejects this new mode with Unsupported
while its existing public behavior remains available.

The scale writer assigns monotonically increasing entry IDs. Each entry stores
a parent directory ID, original basename bytes, kind and observed device/inode.
Source epoch belongs to the inventory; a correction advances it. Device/inode
alone is not a durable object identity across missing observations or offline
periods. A hard link has another entry ID and another searchable path even when
its observed device/inode matches. Removed IDs are not reused within an epoch.
Unknown incoming directories and ambiguous object identity require correction
rather than inferred continuity.

Immutable snapshots share 1,024-record segments and their basename arenas using
Arc. Writer lookup keys are parent ID plus name hash; collisions always compare
the original bytes and use separate collision buckets. Lookup remains owned by
the single writer and is not cloned into query snapshots or ordinary updates.
Paths are reconstructed from the parent relationships of the leased snapshot.
A directory rename edits its directory entry rather than rewriting every
descendant full path. Native watch topology, derived ancestry signatures and
optional sorting can still require subtree work; this does not promise an O(1)
complete rename operation.

Scale pages use increasing entry-ID order, skip deleted slots, and bind cursors
to both the immutable snapshot and exact query text. A cursor does not retain a
snapshot by itself. At most eight leases and one retired snapshot are retained;
new publication pauses when readers pin the previous retired snapshot. Older
leases preserve their earlier paths and are explicitly not validated against a
newer observed version. The existing bounded ordering remains unchanged.

Initial scans are resumable with a configurable entry batch. Each directory is
watched before its listing is read; the source drains between batches and again
before a complete, generation-matched snapshot is published. Local file changes
edit touched segments. Unknown/moved-in directories use conservative correction
in this milestone. Root replacement fails the source while previous immutable
queries remain usable with a failed status.

Public metrics report touched entries and copied segment/record counts for the
last local inventory transaction. They exclude derived path/query cache and
native watch topology work. This distinguishes local updates from inventory
size without claiming an unmeasured resource or latency target.

This milestone retains the bounded scan/watch caps, UTF-8 query requirement and
control exclusions. Raw-name/link/scope configuration, larger watch budgets,
scale persistence, recovery/resource scheduling and query optimization are
separate dependent tasks. Existing databases are rejected and preserved when
opening scale mode; saving a scale checkpoint returns Unsupported. No format is
silently migrated. The small native tests executed on overlayfs establish the
model and API behavior only, not million-entry, ext4/Btrfs, Windows native,
24-hour or RSS/performance acceptance.

## Raw-name and source-scope extension (LOCI-LINUX-001-05)

Scale mode now accepts original basename bytes, including non-UTF-8 bytes. The
shared query matcher searches lowercase valid UTF-8 runs without joining across
invalid bytes; distinct AND terms may match different valid runs. Extension
filters examine the valid filename suffix. Symlinks, including dangling links,
are snapshot entries with `EntryKind::Symlink`; enumeration never follows them.
`QueryLease::entry_kind(absolute_path)` obtains kind from the pinned scale
snapshot, without consulting the current filesystem. Its initial lookup is linear;
bounded compatibility snapshots return Unsupported.

Scale exclusions are explicit relative subtree paths, defaulting to empty.
The control implementation keeps its historical exclusions. Selected database
output and temporary saves remain outside the monitored root. Nested mounts are
included as directory boundary entries but their contents are excluded from the
single-root source. Linux mount IDs and escaped raw paths from mountinfo detect
bind mounts even when device/inode matches. The selected root is bound to the
mount of the originally opened root descriptor; rebinding it fails the source.
A nested mount table change invalidates current coverage and starts correction.
Unavailable, over-budget or unrecognized mount metadata fails explicitly and
preserves the older query snapshot. Mount table polling is currently conservative;
later scheduling/idle work must budget its overhead. It does not promise detection
of an attachment removed again entirely between observation cutoffs.

The CLI routes root/database/exclusion arguments as OS strings and supports
`--scale --exclude RELATIVE_PATH`, with exact raw-byte NUL export. Query text stays
UTF-8. Native scope evidence includes same-device bind mounts and root rebinding
in a private user/mount namespace; these overlayfs tests do not establish ext4 or
Btrfs scale acceptance.

## Responsive query extension (LOCI-LINUX-001-12)

Scale snapshots now include derived segment filters: a 128-bit trigram signature
and a 128-bit adjacent-byte-pair signature per slot, plus one byte/pair union
filter per 64 slots. False positives go to the exact matcher; no filter decides
that a result matches. Normalized ancestry is cached only for directories. Query
verification reuses one scratch buffer and constructs original raw paths only
for returned entries. Invalid UTF-8 runs remain separated by byte FF during
normalization; valid query text cannot cross that separator. Ancestor, slash,
AND, Unicode lowercase expansion and extension semantics remain authoritative.

Derived filters are snapshot-owned Arc segments. File updates copy touched
filter segments; a directory move rederives its affected subtree and replaces
cached ancestry while older leases retain the old view. It retains the entry ID
and source epoch. This subtree work and native watch topology work are separate
from the public metrics for primary inventory parent/name edits. Checkpoints
persist the canonical raw graph and rebuild derived state after graph validation;
parent-first compaction derives incrementally as it inserts. Derived allocations
and directory cache COW are charged before allocation against inventory snapshot
and retained byte budgets. The checkpoint format remains LOCISCL1.

Immediate pages retain entry-ID order and stop at the requested page size.
Independent exact-count and optional lexical-sort jobs pin a single snapshot.
Count streams matching IDs; sort retains only IDs and merge scratch rather than
all reconstructed paths. Sort comparisons use raw filesystem bytes, with stable
ordering during cancellation, and cancellation checks between 1,024-ID sorting
chunks and each 256 merge steps. Completed sorted results provide offset pages
and retain their lease until dropped. Count drops its lease on completion.

At most two query workers run per process, independently of the lease limit.
Sort ID/merge capacity has an explicit 16 MiB default budget and each worker
admits an additional 512 KiB scratch reservation into the shared 4 GiB conservative
process capacity policy. Reservations describe capacity, not RSS or kernel watch
memory. Cancellation and monitor stop prevent active jobs publishing a partial
exact total; direct immutable leases remain readable after stop. Bounded/Windows
pagination and matching are unchanged; optional sorting explicitly requires the
Linux scale mode. The 100k measurements are documented separately and do not
establish million-entry, native Windows, filesystem or RSS acceptance.
