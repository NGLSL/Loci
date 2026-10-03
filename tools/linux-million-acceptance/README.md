# Opt-in real filesystem measurement

Build the release example from a clean exact commit:

    cargo build --release --example linux_million_acceptance --locked --offline

The executable contains both controller and worker. The controller spawns the
same binary in worker mode; exactly one public Engine/MonitorOwner runs in that
separate PID at its production20ms cadence. Independent oracle and /proc sampling
remain in the controller process. No private source or virtual clock is used.

Start a development smoke on an existing verified-owned real fixture:

    target/release/examples/linux_million_acceptance --controller \
      --root /absolute/fixture/data --database /absolute/run/index.loci \
      --output /absolute/fresh-run/results --sha EXACT40HEX \
      --queries /absolute/repo/tools/linux-million-acceptance/queries.txt \
      --phase smoke --smoke --repetitions 1 --idle-seconds 1

The archived UTF-8 newline query file has52 cases: the original32, the complete
ticket12 39-query suite, and short-negative ii/ia/rr cases, without dropping any.
Its initial blank line is the empty query. JSON query arrays are not accepted.
First pages use snapshot ID order; optional sort uses raw-byte lexical order.

After all timed query loops, `--phase queries` independently traverses the raw
native paths once and filters that bounded disk oracle for every query. It checks
the first 50 subset and cardinality, complete typed paginated export, and exact
asynchronous count against the same validated version. Empty, `report`, and
`invoice ext:txt` queries also compare complete raw lexical sort results. Count
and sort completion timings are separate from first-page timings; every job is
dropped before the next case, and successful temporary sets are removed only
after their hashes are logged. A mismatch keeps its raw evidence.

The oracle does not use Engine matching or inventory internals. Terms use AND
substring matching across lowercase valid UTF-8 runs; they cannot cross invalid
bytes. `ext:` is case sensitive, and its last occurrence selects the extension.
OR and NOT remain ordinary literal terms. For a wholly valid path, extension
normalization follows the whole-path context; an invalid path uses the independently
lowercased valid raw basename suffix, matching the established raw-query contract.

Full measurement interface (opt-in, substantial IO/runtime):

    target/release/examples/linux_million_acceptance --controller \
      --root /absolute/fixture/data --database /absolute/run/index.loci \
      --output /absolute/fresh-run/results --sha EXACT40HEX \
      --queries /absolute/repo/tools/linux-million-acceptance/queries.txt \
      --phase all --repetitions 200 --idle-seconds 600 \
      --output-budget-mib 4096

Every run creates a new output ownership marker and refuses to overwrite logs.
Database/output stay outside the chosen root. The actual million baseline is
1,000,000 entries, while the scale default live cap1,250,000 permits a real
ordinary add to1,000,001 before untimed removal. Other physical/memory/watch caps
remain separate. Resource samplers never read20k fdinfo records per timed query;
a separate controller thread samples the bound worker PID at fixed intervals.

Raw timings.jsonl and resources.jsonl retain samples, failed observations and
actual process RSS/HWM/CPU/watch/fd counts. Protocol.md documents bounded binary
IPC, asynchronous job acknowledgements, typed raw-NUL exports and stop proof.
An Engine process includes its IPC/query/thread overhead in measured RSS.
Kernel slab bytes stay explicitly unavailable without an independent measurement.

The run aggregate artifact limit defaults4GiB, configurable512..65536MiB.
320MiB is reserved for bounded timing/resource logs; exports/oracles/kinds/sort
spill require prior admission and periodic usage checks, plus actual filesystem
15%/64MiB reserve. Per-file512MiB is separate. Complete successful cuts retain
SHA256/bytes/count/version receipts, initial+latest canonical sets and original
timings; only owned successful duplicates/older artifacts are deleted. Failure
keeps the current raw partial/mismatch sets. Explicitly enlarge budget before a
new run if needed; never erase failed evidence to make a gate pass.

fixture.py creates the varied real zero-byte ASCII/Chinese fixture and a separate
independent byte-path oracle; process_sampler.py is a standalone external sampler.
Fixture creation is deliberately opt-in with ownership and inode/data guards.
Existing Btrfs inode counters can be zero by design; new Btrfs corpus creation
requires physical metadata DUP pilot/unallocated-capacity guards before execution,
rather than pretending0freeinodes means exhaustion or skipping resource guards.

Measurement completion is distinct from acceptance. A cloud overlay result
cannot establish ext4/Btrfs or the localSSD/NVMe reference class. Numerical failures
and unsupported/unverified gates remain explicit. Native filesystem runners,
exact-commit Windows validation and uninterrupted24h supervision are separate
gates. Do not infer a million performance result from the small protocol tests.

`--phase stages` runs full byte/kind comparisons around a real top-level subtree
rename, exports a held old lease, restores the fixture, waits for actual
compaction publication (epoch and completed counter), exercises query and
correction cancellation, and measures three complete corrections independently.
`--phase directory-heavy` runs those stages on the owned dense fixture with
30,501 entries; it records a separate stress result rather than a million-entry
headline. A reduced custom fixture requires `--smoke`.

The manifest binds the supplied source SHA, executable SHA256, query-file SHA256,
kernel/CPU/memory facts and configured run options. Samples include actual
`/proc/PID/io` counters when readable. The aggregate output budget includes the
external checkpoint file; 320 MiB is separately reserved for bounded logs.
Successful comparisons remove owned duplicate files after recording hashes;
failed sets remain available.

A small real worker protocol check is available separately:

```sh
python3 tools/linux-million-acceptance/protocol_smoke.py \
  --binary /absolute/target/release/examples/linux_million_acceptance \
  --sha EXACT_40_CHARACTER_SOURCE_SHA --output-parent /owned/absolute/directory
```

It checks native capture, invalid UTF-8, symlink/hardlink kinds, a held snapshot,
exact count/sort, and release of watches and descriptors in the same live PID.
It establishes neither million-entry performance nor reference hardware acceptance.

Read-only evidence assessment for the archived standard 52-query million fixture:

```sh
python3 tools/linux-million-acceptance/assess.py /absolute/run/results \
  --sha EXACT_40_CHARACTER_SOURCE_SHA > /absolute/run/assessment.json
```

The script reads retained JSONL and completion metadata; it neither runs workloads
nor reimplements the oracle. It checks numeric thresholds from reported values and
lists each requirement as `passed`, `failed`, or `unverified`. A stages-only run
leaves query/event/save200/restart200/600-second gates unverified. Smaller or dense
fixtures do not satisfy the standard million fixture check. Assessment is evidence
for review: it never closes an issue, establishes reference hardware/native
filesystem/Windows/24-hour acceptance, or treats exit zero as product acceptance.
Its own successful exit means the evidence was read, including failed findings.
The event gate requires the driver's canonical `add`, `delete` and `file-rename`
classes, with at least 200 samples each and p95 at most 500 ms. Missing evidence
remains unverified; unknown or incomplete class sets cannot pass.
