# Linux compaction and idle development evidence

Production source: `57cca5b51c217db1966fb15a88256b5cf7fdb2ee`.
Release engine probe SHA-256:
`59c26be0300053e355a852e240310f1ea086f8ee6d90d5f0a1ccb92e60ceb6be`.
The standalone driver links the production library and calls public `Engine`,
`MonitorOwner`, immutable query leases, and compaction APIs. Its owner uses the
production 20 ms polling interval. The controller and oracle run in another PID.

## Environment and datasets

2026-10-02, x86_64 Linux 6.18.44, Intel Xeon Platinum 8573C, five available logical
CPUs, 18,440,136 KiB total RAM, UID/GID 1000, Rust 1.99.0 release, overlayfs.
The million-entry fixture has exactly 20,000 directories and 980,000 real files;
directory depths are 1–3 and maximum relative path length is 54 bytes. Default
scale limits build all 1,000,000 non-root entries plus the internal root slot.
The independent controller recursively lists native byte paths and compares the
complete sorted set to public paged exports, including all entries after the
first 50. The million-entry export matched all 1,000,000 paths and the native
session installed 20,001 watches, including the root watch.

Event trials use a separate 100,000-entry/2,000-directory real fixture. Each class
has 20 warmups and 200 recorded trials. Unique owned files isolate operations.
Elapsed time starts immediately before the actual create/delete/rename syscall
and ends when a public query returns the exact expected path set on a validated
cut. It includes production owner polling, query work, and controller IPC.

| Native event | Timed trials | p50 ms | p95 ms | p99 ms |
| --- | ---: | ---: | ---: | ---: |
| Add | 200 | 19.976 | 22.563 | 23.003 |
| Delete | 200 | 21.981 | 22.383 | 22.699 |
| Rename | 200 | 22.051 | 22.366 | 23.308 |

Full 100,000-path exports equal the independent oracle before the trials, after
the trials, and after explicit incremental compaction. Full-root scan counts do
not increase. The million-entry dataset is used for quiet measurement without
mutating the exact-cap fixture.

## Actual engine process idle window

Engine PID 86293 is sampled externally through `/proc/PID/stat`, `status`, and
`fdinfo`, at five-second intervals. CPU is process user+system ticks divided by
real elapsed time, expressed as percent of one logical core. There is no harness
CPU/RSS subtraction. The completed window runs from 2026-10-02 21:41:54.312 UTC
to 21:51:54.400 UTC, for 600.087612 real seconds, with 120 samples.

| Actual engine process measurement | Result |
| --- | ---: |
| CPU, percent of one logical core | 0.281626% |
| RSS, minimum and maximum | 176,570,368 bytes (168.39 MiB) |
| Process HWM, including build/export | 192,757,760 bytes (183.83 MiB) |
| Kernel watches during idle | 20,001 |
| Actual process descriptors during idle | 5 |
| Full-root scans, before and after | 1 → 1 |
| Scanned entries, before and after | 1,000,000 → 1,000,000 |
| Audited directories | 0 → 1,920 |
| Full scope-table checks | 14 → 611 |
| Actual kernel watches after stop | 0 |
| Actual process descriptors after stop | 3 |

Version 1 remains Validated, queues remain empty, and inventory capacities remain
unchanged throughout the window. The process sampler's generic acceptance flag
remains false; this document establishes the specific development idle gate from
its completed-window and actual-process measurements. The separate 100k event
process finishes compaction at epoch 2, reclaiming 660 physical slots and 48,620
obsolete name bytes while its full-root scan count remains one.

Capacity accounting is separate from process memory: the initial million-entry
snapshot reports 99,881,449 bytes; deduplicated retained inventory capacity is
99,897,177 bytes. Process-wide conservative admission reserves 1,669,281,840
bytes for the configured owner envelope, including writer/candidate/retired data,
queues, watches' userspace paths, and scratch. That reservation is an upper-bound
capacity policy, not RSS or live malloc payload. Kernel watch/slab bytes are not
available; native watch and fd counts are measured directly.

## Regression and recovery checks

Public production-owner regression: 60 actual files, `scan_batch=1`, a compaction
spanning many 20 ms polls, and three new files each visible within 500 ms. Old
leases, cancellation, resumed compaction, checkpoint/reopen, and full-set equality
remain checked. A simulated external source delivering consecutive trusted
refresh batches against real files verifies both batches publish without an
extra root scan. Native overflow, watch/source identity loss, rename expiry,
permission loss, and mount-scope correction retain their existing gates.

The former quiet run is retained as interrupted evidence: source `f3986dd`,
100,000 entries, 365.60 seconds, approximately 1.012% CPU of one core. It was
stopped after locating an unconditional watch-path-map clone on empty capture;
it is not a completed ten-minute idle result. The corrected empty-batch path
follows capture/loss/drain/rename-expiry checks. Loss detection remains covered
by the actual kernel-overflow regression.

Focused development checks: compaction 9, recovery 7, scope 6, checkpoint 9,
budgets 5: 36 passed, one explicit 100k fixture test not selected. Rustfmt and
all-target compilation pass. Integration also passed its 30 necessary focused
checks. This is not a full-suite or final unified review record.

## Raw evidence and limits

Workspace artifacts: `/workspace/linux-ticket-13-measurement-57cca5b/` contains
`events.jsonl`, `million-oracle.json`, `idle.jsonl`, its summary, environment and
driver hashes, and process logs. The standalone source is
`/workspace/linux-ticket-13-engine-probe.rs`, controller is
`/workspace/linux-ticket-13-measure.py`, and sampler is
`/workspace/linux-scale-process-sampler.py`. The sampler's legacy `run_id` says
100k after the controller switches processes; `run-label-erratum.json` records
that label error without changing raw samples. PID, oracle count, root, and watch
count establish the actual million-entry idle dataset.

These are native inotify observations on development overlayfs. They do not
establish the final SSD/ext4/Btrfs million-entry acceptance, million-entry event
latency under compaction/churn, or long-run endurance gates.
