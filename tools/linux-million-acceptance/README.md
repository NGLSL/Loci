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
