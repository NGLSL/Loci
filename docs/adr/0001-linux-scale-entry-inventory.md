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
