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
`rebuild` reopens the same public Engine and reconciles the complete selected root;
it does not silently discard damaged databases. A damaged or incompatible database
fails explicitly and remains available for recovery.

Query terms use the existing case-insensitive AND substring and exact extension
semantics. Results alone go to stdout. Default output quotes and escapes each path
as a Rust string; `--null` returns exact UTF-8 path bytes followed by NUL, suitable
for tools accepting NUL-delimited paths. Diagnostics go to stderr and include the
snapshot version, completeness, validation status, match count, and returned-path
count. Queries currently return at most fifty paths; full enumeration is a separate
follow-up. Snapshot completeness and validation describe different things.
`Validated` describes the last reliable observed filesystem cutoff, while
`Pending`, `ReadersPinned`, or `Failed` can retain older searchable results.
They never claim those results are current. `Stopped` confirms release of ownership.

The existing v0.1 limits still apply: 4096 entries, 128 directories and 1 MiB of
UTF-8 paths. Unsupported raw-byte filenames and implicit experimental exclusions
remain limitations of that bounded Engine. The database parent must exist and both
the database and temporary saves must be outside the selected root. Every one-shot
command opens and reconciles the root; this CLI does not yet offer fast stale-start
loading or a separate background query service.

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
