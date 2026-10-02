# Real-directory bounded CLI

The `engine` namespace uses the public native Engine and a user-selected directory.
The original `build N db`, `bench`, `query db query`, `scan`, and `live-check`
commands remain experiments; `build N` creates synthetic strings.

```sh
cargo run --release -- engine build /path/to/data /path/outside/data/index.loci
cargo run --release -- engine query /path/to/data /path/outside/data/index.loci '中文 report ext:rs' --null
cargo run --release -- engine status /path/to/data /path/outside/data/index.loci
cargo run --release -- engine rebuild /path/to/data /path/outside/data/index.loci
cargo run --release -- engine watch /path/to/data /path/outside/data/index.loci --null
```

`watch` continuously polls the native Engine while accepting newline-delimited
commands on stdin: `query QUERY`, `status`, `save`, `rebuild`, and `stop`.
An empty `query` searches all entries. Type `stop`, or close stdin, to save and
release monitoring. It installs no system service. A bounded eight-command queue
and 4096-byte command limit prevent terminal input from growing without a bound.
`rebuild` requests fresh correction from the scale Engine, or reopens the bounded
Engine, and reconciles the complete selected root. A damaged or incompatible database
fails explicitly and remains available for recovery.

Query terms use the existing case-insensitive AND substring and exact extension
semantics. Results alone go to stdout. Default output quotes and escapes each path
as a Rust string; `--null` returns exact UTF-8 path bytes followed by NUL, suitable
for tools accepting NUL-delimited paths. Diagnostics go to stderr and include the
snapshot version, completeness, validation status, match count, and returned-path
count. Default queries return at most fifty paths; `--all` enumerates the complete
snapshot through bounded pages. Snapshot completeness and validation describe different things.
`Validated` describes the last reliable observed filesystem cutoff, while
`Pending`, `ReadersPinned`, or `Failed` can retain older searchable results.
They never claim those results are current. `Stopped` confirms release of ownership.

The existing v0.1 limits still apply: 4096 entries, 128 directories and 1 MiB of
UTF-8 paths. Unsupported raw-byte filenames and implicit experimental exclusions
remain limitations of that bounded Engine. The database parent must exist and both
the database and temporary saves must be outside the selected root. Bounded commands
open and reconcile the root. Scale commands can return saved results before correction;
`watch` drives correction while accepting queries and status commands.

Exit codes are 0 for success, 2 for invalid arguments or unsafe database placement,
3 for failures, and 4 for a pending/incomplete observation. A watch command error
is reported and leaves the session running; a terminal-read error terminates it.
Normal stop/EOF attempts to save a validated snapshot; failure exits nonzero rather
than storing an incomplete snapshot. Use `engine --help` for the command summary.

Snapshot pagination and full export:

```sh
cargo run -- engine query /chosen/root /outside/root.loci 'ext:txt' --all --page-size 128 --null
# In a watch session, `export ext:txt` exports the pinned snapshot in 50-entry pages.
```

The public `Engine` query lease provides `page(query, cursor, page_size, cancel,
progress)`. Sizes are 1–1024 (default 50). `QueryPage.next` is an opaque token for
that immutable snapshot and exact query text. Keep the lease while paging; a
newer published snapshot never changes its results. A cursor used with another
snapshot, another engine, or another query returns `InvalidInput`; a cursor alone
does not retain a snapshot. Dropping the lease promptly releases the existing
bounded reader budget (eight leases and at most one retained old version).

Pages use stable snapshot storage order: flat snapshots enumerate lexical paths;
partitioned snapshots enumerate partition IDs, then lexical paths within each
partition. This differs from the legacy lexical first-fifty search, whose behavior
is preserved. Enumeration may return a final empty page if a previous full page
was followed by records that do not match. `complete` means the snapshot
enumeration is exhausted, while `validated_at_start_and_finish` independently
reports whether the monitored view still validates that snapshot. Cancellation
returns partial paths and a continuation at the next unvisited record; retry with
that token and a cleared cancellation flag. Progress counts records visited in
this page. No exact total count is needed to produce a page.

`--all` writes every page to stdout and page/version/validity diagnostics to stderr;
`--page-size` requires `--all`. Output preserves exact NUL-delimited path bytes
with `--null`. A failed or pending export exits 4 and can have partial output;
callers requiring an atomic output file should stage it and check the exit code.

Scale raw-name and scope mode:

```sh
cargo run -- engine query /chosen/root /outside/unused.loci '报告 ext:txt' --scale --exclude private --all --null
```

All `engine` commands accept `--scale` and repeatable `--exclude RELATIVE_PATH`.
Exclusions omit the named relative entry and its descendants, default to empty,
and require scale mode; `.git` and `target` are included unless configured.
Root, database and exclusion arguments preserve OS bytes. Query text must be
UTF-8. Legal UTF-8 runs in unusual names remain searchable with lowercase AND
matching, but a single term never matches across an invalid byte. Display quotes
escape invalid bytes as `\xNN` and newline as `\n`; `--null` preserves all original
path bytes without shell interpretation. Symlinks are listed without following
external targets, dangling targets, or cycles.

The selected root uses one mount source. Nested mount directories are included
as boundary entries and contents are omitted, including same-device bind mounts.
Mount changes invalidate coverage for correction; selected-root rebinding or
unknown mount metadata reports a failure rather than validated coverage.
`watch` and its `rebuild` command retain the selected scale/exclusion options.
Scale checkpoints use the separate LOCISCL1 format. Build/save and watch stop
persist coherent validated snapshots. LOCISNP1 databases require a separate scale
rebuild destination; unknown or damaged databases are preserved. Loaded results
are Pending until correction completes. The bounded format and limits remain the
default. Complete export/build/rebuild wait for Validated; ordinary first-page
queries can return explicitly stale saved results with exit code 4.

For scale query/status, `--fresh` requests full correction before returning results.
Without it, a verified saved checkpoint is immediately searchable as `Pending`,
including entries deleted or renamed while stopped. Root/source/mount/scope identity
is checked and a native root watch is installed before this return. Linux has no
generic persistent event cursor: a loaded checkpoint is never assumed current.
The subsequent correction watches each directory before enumeration, drains native
events between bounded batches and publishes only a reliable complete candidate.

```sh
engine query /chosen/root /outside/root.loci 'report' --scale --null
engine query /chosen/root /outside/root.loci 'report' --scale --fresh --null
engine status /chosen/root /outside/root.loci --scale --fresh
engine watch /chosen/root /outside/root.loci --scale --scan-batch 256 --null
```

Startup stderr reports `phase=stale,first_searchable_ms=...`, then
`phase=correcting,scanned_entries=...`, and finally
`phase=validated,full_correction_ms=...`. Both elapsed times start immediately before
opening the Engine. The first measures opening/loading and obtaining a searchable
snapshot; the second measures the total elapsed time until validated coverage.
Cold starts can first become searchable already validated. Progress diagnostics are
throttled to at most one per 100 ms after the initial correction marker. These are
per-run measurements, not a demonstrated million-entry latency bound.
`watch-ready` can precede correction completion, so queries can return saved paths
with `ok=false`/Pending until corrected. `cancel` pauses correction while retaining
those paths; `rebuild` resumes it. Failure or cancellation never permits saving
unvalidated results over the checkpoint. Stopping while Pending preserves the saved
database and reports the unsuccessful save through the existing nonzero exit code.

Scale native monitoring allows an explicit watch budget (`EngineOptions.watch_limit`,
default 32,768 including the selected root), with a hard shared process ceiling of
65,536 actual watches across bounded and scale owners. Bounded owners retain their
additional shared limit of 128. All owners share eight native inotify descriptors;
these counts describe inotify descriptors, rather than every descriptor in the
process. `EngineOptions.event_limits` bounds native queued events and read-buffer
bytes; actual queued bytes/counts are reported separately from the configured limit.

Scale status exposes `View.coverage_gaps` (exact affected path, reason and original
errno) and `View.resources`; CLI status writes these details to stderr. Permission
failure, a watch budget gap or kernel ENOSPC never publishes Validated coverage.
Previously published snapshots stay searchable and explicitly unvalidated. Initial
partial coverage produces an inspectable Failed Engine with no published snapshot.
Retries wait 250 ms and stop after four failures; restoration before exhaustion can
recover, while an explicit reopening/rebuild restarts an exhausted attempt. No system
watch limits are changed. Native source replacement and stop/drop release resources.
