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

## Typed page export and live capacity extension (LOCI-LINUX-001-14)

Million-entry short-query tuning widens only the scale per-entry adjacent-byte
pair filter to 256 bits. The 128-bit trigram filter and 128-bit pair/byte block
union remain unchanged. Queries derive the wider filter once; false positives
still pass through the exact normalized matcher. Legacy serialized control
signatures retain their original hash positions and format. LOCISCL1 continues
to store the canonical raw graph and rebuilds derived filters after restoration.
The additional 16 bytes per physical slot join conservative owner admission;
actual filter capacities, COW candidates and retained segments are charged by
their sizes against the existing snapshot/retained limits before allocation.
No full path cache is added for files.

QueryPage.kinds is optional, positionally aligned with paths. Linux scale normal
and completed-sort pages collect kind directly from matching immutable entry
IDs. This avoids the existing linear QueryLease::entry_kind(path) lookup in a
complete million-entry export. The path-based API remains compatible; bounded
and Windows page results have None. Held older pages preserve their paths and
kinds after directory moves or kind replacement in newer publications.

Scale default live capacity is1,250,000 entries, supporting an actual new file
above an exact million-entry reference inventory. This does not raise physical
slot/name/snapshot/process/watch caps, migrate checkpoint formats, or imply that
the performance gates already pass. LOCISCL1 graph validation uses the configured
live budget; bounded Engine validation remains separate and unchanged.

## Scale buffer layout and page reclamation

Linux scale record segments, basename arenas and derived filter arrays use
private anonymous read/write mappings. Their containing Arc retains ownership
through older snapshots and query jobs. Copy-on-write allocates a separate,
fallible mapping; the old mapping stays readable until its last Arc drops, then
munmap returns its pages directly. This does not map corpus or checkpoint files.
The page size is observed through Linux sysconf. Snapshot/retained accounting
charges page-rounded mapping capacity and buffer metadata before allocation;
name-arena growth also admits the old/new transient overlap before copying.
Shared mappings remain deduplicated in retained accounting. These capacity
credits are distinct from actual process RSS and kernel memory.

Writer lookup keys preserve the complete parent u32 and basename hash u64 in a
12-byte key, avoiding tuple padding without truncating either value. Entry IDs
already require checked u32 conversion; sibling positions also use checked u32
conversion under the configured live-entry bound. Collision buckets still
verify original basename bytes. Writer child lists retain up to 64 IDs inside
their hash-table buckets, with Vec overflow for larger directories. Directory
prefixes retain up to 128 normalized bytes inside snapshot cache buckets, with
immutable Arc<Vec> fallback for longer ancestry. No full file-path cache is
introduced. Prefix and adjacency bucket capacities join their respective
snapshot/retained and conservative two-writer process admission bounds.

GNU Linux additionally requests allocator maintenance after dropping at least
16 MiB of uniquely owned scale allocations. A final drop-guard field records
that request after its owned buffers actually finish dropping. Active scale
poll/stop boundaries drain the atomic request with malloc_trim(0), outside the
shared snapshot mutex. Ordinary small COW updates do not meet this threshold;
a quiet poll only checks the atomic flag. This releases unused GNU allocator
pages without moving live buffers. Other Linux libc builds use direct mapping
release without the GNU trim call. Default bounded Linux and Windows layouts
and behavior remain unchanged; Windows scale mode remains Unsupported.

Complete filesystem correction reuses the unique writer lookup, collision,
child-list and position-container capacities for its new candidate. It clears
these indexes before enumeration and keeps the old immutable Data/query cut.
Events are not applied to the retired writer while correction is pending; a
cancelled or failed candidate preserves old searchable/saveable data and requires
correction before ordinary updates resume. Compaction still uses an independent
writer because it traverses the old graph. This avoids repeated large allocator
releases during correction without changing admission or snapshot/lease limits.
It does not establish that the steady RSS gate passes; complete lifecycle
measurements remain required.

The development lifecycle measurements distinguish mapped/cached capacities,
GNU allocator chunk statistics and actual engine-process RSS. GNU mallinfo2 in
the opt-in measurement example is read-only diagnostics, requires a supporting
GNU libc, and does not report live Rust payload or RSS. The production library
has no mallinfo2 dependency. See [memory evidence](../LINUX-MILLION-MEMORY-RECLAMATION.md)
for complete failed runs, successful-cut requirements and environment limits.

## Reliable new directories and move scope

The final scale implementation supersedes the milestone's unconditional
correction for a new directory beneath an already known parent. The single
writer installs the watch before opening each listing and enumerates only that
new subtree in bounded batches. It does not clone writer lookup state or scan
unrelated directories. The existing published cut stays queryable while local
work is pending; publication waits for complete enumeration and trusted pending
updates. Loss, ambiguous or unknown directory rename origins, changed source
identity, scan failure and cancellation retain the established correction and
coverage-gap contracts. A moved-in unpaired native rename remains observation
loss and requires correction.

Before a reliable rename edits writer relationships or removes its destination,
it checks the resulting live subtree against the configured depth and 4096-byte
full-path bound. A violation reports the affected path, keeps the previous
query cut, and cannot publish Validated. Checkpoint graph/cycle/parent checks
still cover every physical record. Depth/path scope checks apply to live records:
a deleted child's historical basename cannot make a later valid ancestor move
produce an unreadable checkpoint.

Coverage error producers carry their classification separately from their
human-readable text. Native application watch budgets, kernel errno limits,
permission failures and source-identity changes keep distinct public gap kinds.
