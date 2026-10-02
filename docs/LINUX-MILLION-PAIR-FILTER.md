# Million-entry pair filter development measurement

The canonical real fixture and all reference queries remain unchanged. The
baseline source `b45dd8d955e720ca57cdc489d28e0e48e04e6726` fails the per-query
50 ms p95 gate for `rr` in pass 1. Widening the scale per-entry adjacent-byte
pair filter from 128 to 256 bits reduces false candidates and brings every
query/pass below the target in the measured development run.

## Mechanism and bounds

Diagnosis reads actual relative component paths from the independent million-entry
byte oracle. The dataset contains no `rr`, `ia` or `ii` match.

| Query | 128-bit pair candidates | 256-bit pair candidates | Actual matches |
| --- | ---: | ---: | ---: |
| rr | 233,958 | 17,172 | 0 |
| ia | 252,951 | 26,746 | 0 |
| ii | 32,114 | 13,988 | 0 |

The wider filter uses two positions across 256 bits. Collisions admit extra
candidates; the existing exact normalized matcher decides results. Query filters
are derived once. The 128-bit trigram filter and legacy byte/pair block union
remain unchanged. Legacy serialized signatures retain their hash positions and
format. Scale checkpoints still store the raw graph and rebuild derived state.
Files acquire no permanent full-path cache.

The added capacity is 16 bytes per physical slot, including rounded segment
capacity. Size-based allocation credit covers growth and copied/retained filter
segments before allocation. Conservative owner admission adds 16 bytes per
configured maximum slot. Existing physical, snapshot, retained, sort-job and
process limits are retained.

## Same reference workload

Measured optimized source: `86d2941eb43e155d47b3b4dc744d78ca09a2ca94`.
Release foundation executable SHA-256:
`13e144dd45a24fcb2871d26b4cddc662703287400ec7ae572218896466545617`.

Both runs use `/workspace/loci-million-varied-20261002/data`: 1,000,000 actual
non-root entries, comprising 20,000 directories and 980,000 files, depths 1–3.
Both initial complete byte-path and kind exports equal the independent oracle.
The production owner runs in a separate worker PID at its 20 ms cadence; actual
worker watches are 20,001. Controller/oracle CPU and memory are excluded from
worker-process resource readings.

Both runs preserve all 52 queries in
`tools/linux-million-acceptance/queries.txt`, including its empty first query;
SHA-256 `53b6379f77d0fb79c134285b92382347a2c029d598f5278ad2d011eae4b99a74`.
Five warmups per query precede two passes of 200 samples per query, in the same
rotating order. Each run retains 20,800 timed samples and 104 individual
query/pass summaries. Timings include the worker's public handle, lease and page
operation. The p95 gate is applied independently to each query in each pass.

| Query/pass | Baseline p95 ms | Wider filter p95 ms |
| --- | ---: | ---: |
| rr / 0 | 49.855 | 11.430 |
| rr / 1 | **51.468 (fail)** | 12.318 |
| ia / 0 | 47.248 | 13.392 |
| ia / 1 | 48.049 | 14.066 |
| ii / 0 | 11.815 | 13.145 |
| ii / 1 | 12.461 | 14.020 |

All 104 optimized query/pass p95 checks pass. The worst individual p95 is
`dir00000`, pass 1, at 14.837 ms. The `ii` slowdown is retained explicitly rather
than hidden by an aggregate. The optimized worker PID is 97240. Its build takes
9.35 seconds; the full measurement, exports and save finish in approximately
67.6 seconds.

| Actual worker / inventory metric | Baseline | Wider filter |
| --- | ---: | ---: |
| Steady query RSS, maximum | 177,008,640 B (168.81 MiB) | 192,962,560 B (184.02 MiB) |
| Process HWM, including save | 225,628,160 B (215.18 MiB) | 241,643,520 B (230.45 MiB) |
| Snapshot allocated capacity | 99,881,449 B | 115,888,617 B |
| Deduplicated retained capacity | 99,897,177 B | 115,904,345 B |
| Conservative owner reservation | 1,765,281,840 B | 1,797,281,856 B |

The optimized run meets steady RSS ≤200 MiB and peak HWM ≤512 MiB for this phase.
Capacity reservations are distinct from RSS and actual allocator payload. Native
kernel watch/slab bytes remain unavailable; actual watch/fd counts are sampled.
Same-PID stop joins the owner and leaves three actual descriptors and zero kernel
watches.

## Checks, evidence and remaining gates

Focused public checks cover all 676 ASCII letter pairs, ancestry/slashes,
invalid-byte separation, Unicode lowercase expansion, extensions, old directory
leases, sorting/count jobs, cancellation, checkpoint rebuild and budget failure.
The opt-in direct-owner 52-query latency test is also preserved. Its unchanged
baseline passed narrowly (`rr` 49.865/48.306 ms); it is supplemental evidence,
not a new failing test. The original foundation worker's 51.468 ms failure is
the performance failure used for comparison.

Original failure artifacts remain at
`/workspace/linux-ticket-14-queries-b45dd8/results`. Optimized raw samples,
resources, oracle receipts, completed measurement and before/after summary are
in `/workspace/linux-million-query-wide-86d2941-01/results`. The separate direct
test logs and their result-label metadata remain outside the repository. The
first optimized startup attempt failed before HELLO because its database parent
was absent; that raw attempt is preserved separately and is not a timing run.

This is native inotify on development overlayfs, with reference hardware still
unverified. It is a successful query-performance phase, not full semantic or
ticket14 acceptance. The independent verifier's existing Greek contextual-sigma
extension case under an invalid parent remains a separate production fix.
Integration merged the subsequent dense-correction/resource batching and verifier
helper after this exact-source measurement; final unified-source native/performance
gates, ext4/Btrfs/SSD evidence and endurance remain outstanding.
