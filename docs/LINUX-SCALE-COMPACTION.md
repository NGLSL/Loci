# Scale compaction and idle resource scheduling

Linux scale inventories reclaim tombstones and obsolete basename bytes without
rescanning the filesystem. Automatic reclamation starts when garbage reaches a
quarter of physical slots/name bytes, or queued changes approach those budgets.
`Engine::request_compaction()` and `MonitorOwner::request_compaction()` explicitly
schedule it; acknowledgement does not establish completion. Bounded mode reports
Unsupported. CLI watch accepts `compact`; `cancel` pauses candidate work, and a
new `compact` resumes it. Queries continue against the published snapshot.

Compaction copies live entries in parent-first order into private segments, at
most `min(scan_batch, 4096)` work steps per poll. Its DFS frames use directory
depth rather than entry count. The writer graph is frozen while a candidate is
built. Events invalidate its generation; candidate disposal/restart is bounded,
while the application queue retains its existing item/byte limits. Overflow
requests correction and preserves the last published data. Old leases retain
their original IDs, paths and cursor order. A new compacted epoch can assign new
IDs. A pinned retired snapshot pauses publication with ReadersPinned; allocation
failure preserves the previous query cut and reports failure. Cancellation drops
the candidate. Trusted pending events take priority when the current writer has
slot/name headroom: the candidate is dropped, changes are published, and the
compaction request remains queued. If headroom is exhausted, bounded compaction
must finish before those events can be applied. Cancellation of candidate work
does not pause ordinary updates with headroom. Events captured during application
retain the unpublished writer and are applied on the next poll; reliable batches
alone do not force a root scan. The source is captured again before commit.

`Metrics` reports compaction attempts/completions/restarts, copied live entries,
reclaimed physical slots and obsolete name bytes. These counters are separate
from root scans and do not imply a universal subtree rename latency. CLI status
prints these metrics, epoch and compaction activity.

All scale owners share a 4 GiB conservative capacity admission pool. Admission
precedes native source/root startup and is held by shared query state, including
after the writer stops while handles/leases retain data. The envelope covers the
configured retained Data limit (deduplicated segments, including candidates),
two writers' hash maps/collision buckets/adjacency/positions/directory sets,
native physical/reverse/logical watch paths, scan todo paths, bounded event queues
and buffers, mount-table/decoded scope/configuration/accounting/path/page scratch.
The assumptions and arithmetic are in `engine/scale/memory.rs`. Exclusions are
bounded to 1 MiB. Physical slots, live entries/directories and name arenas keep
their independent limits. Credit precedes candidate root and segment allocation.

Checkpoint encoding computes and admits its exact buffer capacity before growing
it. Loading admits an exact metadata-sized input and conservative graph validation
scratch before allocation; changing file lengths, oversized paths, lengths and
graphs fail explicitly. These transient reservations coexist with owner admission.
Query workers may acquire additional reservations, retained with completed result
buffers. Arbitrary caller-owned EventSource internals and query results after
ownership transfers to the caller are outside the engine-owned capacity envelope.

`memory_reserved_bytes` and `process_memory_reserved_bytes` are conservative
capacity reservations, not live allocator payload or process RSS. Snapshot byte
metrics describe vector/segment capacity, not malloc metadata or kernel memory.
RSS/HWM must be sampled from the actual engine PID. Actual watch and descriptor
counts remain separately reported; kernel watch/slab bytes are unavailable unless
an independent native measurement supplies them. Lower configured limits admit
more owners; exhaustion returns WouldBlock. The bounded compatibility runtime
retains its earlier resource policy.

Quiet polls retain source/root identity checks and cheap mount-ID checks on each
20 ms production owner poll. Whole mount-table parsing occurs at most once per
second during quiet operation; a replaced/masked mountinfo input is checked
immediately. Correction boundaries and actual directory renames force scope
checks. Directory traversal opens and checks mount identity before descent, so a
new bind mount cannot be traversed while the table is cached. Nested scope changes
request correction; a selected-root bind rebind fails on its first poll. Coverage
audits retain their five-second/16-directory default budget. Quiet capture avoids
walking all retained segments or copying the watch-path map. Capture still checks
overflow, bounded drains and pending rename expiry before its empty-batch return.
Raw inventory allocation capacity is cached.

Development performance evidence is recorded separately with exact production
SHA, real wall-clock duration, engine PID CPU/RSS/HWM, native watch/fd counts,
audit activity and production event polling. Overlayfs evidence does not establish
the final ext4/Btrfs million-entry or long-run gates.
