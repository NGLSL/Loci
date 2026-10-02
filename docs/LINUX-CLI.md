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
