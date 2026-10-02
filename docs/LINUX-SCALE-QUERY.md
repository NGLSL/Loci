# Native 100k query baseline

Engine and driver commit: `f3986dd339abe2a54a5cda4ced5056055757be60` (2026-10-02). Linux native Engine on an existing 100,000-entry overlayfs fixture across 2,000 directories. This is a 100k development baseline; million-entry latency, production RSS, ext4/Btrfs, Windows native and long-duration acceptance remain separate gates.

Reproduce against a separately prepared real fixture:

```sh
source /workspace/loci-env.sh
cargo run --offline --locked --release --example linux-query-baseline -- /path/to/100k-root /outside/root/query.jsonl
```

The driver walks the native filesystem and compares complete snapshot pagination for all 39 queries to that full-set oracle, then independently completes exact-count jobs. Small native regression tests use a separate valid-run oracle for Unicode lowercase expansion, invalid bytes, ancestor/slash boundaries, AND and exact extensions. The timed path includes public handle lease acquisition and first-page retrieval of up to 50 entries, with no prior exact count or global sorting. Each query runs 200 times; raw rows retain separate lease/page/total times, returned rows and completeness. Full raw-byte lexical sorting and process worker-admission/cancellation checks run outside timing loops. These warmed repetitions measure one immutable snapshot; they do not include cold index construction or native owner scheduling.

Build and validation took 318.878 ms. All 7,800 timed queries, 39 complete result sets, 39 exact counts and the optional full 100k sort passed their oracle assertions. Aggregate p50/p95/p99: 0.027047/2.786263/4.431977 ms. The worst per-query p95 was `dir00000` at 4.779512 ms; the table preserves every query class rather than relying on the aggregate.

| Query | Exact matches | p50 ms | p95 ms | p99 ms |
| --- | ---: | ---: | ---: | ---: |
| `(empty)` | 100000 | 0.006482 | 0.009308 | 0.037067 |
| `a` | 39200 | 0.032139 | 0.054430 | 0.095429 |
| `b` | 19600 | 0.061601 | 0.107123 | 0.181477 |
| `c` | 39200 | 0.032155 | 0.049427 | 0.087046 |
| `ab` | 9800 | 0.025881 | 0.029323 | 0.042899 |
| `in` | 9800 | 0.024245 | 0.027210 | 0.049069 |
| `re` | 19600 | 0.027159 | 0.029875 | 0.041950 |
| `abc` | 0 | 0.680456 | 0.967314 | 1.111688 |
| `log` | 9800 | 0.024507 | 0.046150 | 0.116850 |
| `pdf` | 9800 | 0.025488 | 0.028441 | 0.045380 |
| `invoice` | 9800 | 0.023033 | 0.035960 | 0.041575 |
| `report` | 9800 | 0.023626 | 0.044497 | 0.092744 |
| `source` | 9800 | 0.023376 | 0.028345 | 0.098732 |
| `报告` | 9800 | 0.027463 | 0.029286 | 0.091953 |
| `告` | 9800 | 0.027384 | 0.030884 | 0.076092 |
| `dir` | 100000 | 0.007176 | 0.010015 | 0.011603 |
| `dir00000` | 50 | 4.331277 | 4.779512 | 5.070162 |
| `dir01999/` | 49 | 0.562273 | 0.696307 | 0.842983 |
| `/dir` | 100000 | 0.006902 | 0.010209 | 0.025952 |
| `ext:txt` | 9800 | 0.022511 | 0.025633 | 0.039319 |
| `ext:pdf` | 9800 | 0.022374 | 0.025362 | 0.037476 |
| `ext:rs` | 9800 | 0.024421 | 0.037829 | 0.048358 |
| `ext:docx` | 9800 | 0.025981 | 0.029273 | 0.042667 |
| `ext:md` | 19600 | 0.021767 | 0.026968 | 0.040522 |
| `invoice ext:txt` | 9800 | 0.024238 | 0.028015 | 0.049129 |
| `report ext:pdf` | 9800 | 0.024172 | 0.025993 | 0.050007 |
| `dir00000 invoice` | 5 | 1.098665 | 1.232783 | 1.374581 |
| `dir01999 source` | 5 | 0.539444 | 0.695285 | 0.868057 |
| `dir00100/ab` | 5 | 0.546234 | 0.665492 | 0.730016 |
| `never_present` | 0 | 0.532887 | 0.646230 | 0.738726 |
| `z` | 9800 | 0.069062 | 0.114528 | 0.181376 |
| `xyz` | 0 | 0.633606 | 0.724653 | 0.836265 |
| `报告 ext:docx` | 9800 | 0.028029 | 0.042765 | 0.062374 |
| `backup` | 9800 | 0.023434 | 0.034363 | 0.056793 |
| `image` | 9800 | 0.023084 | 0.036373 | 0.080109 |
| `video` | 9800 | 0.023326 | 0.026346 | 0.045425 |
| `ii` | 0 | 0.987887 | 1.134889 | 1.284958 |
| `ia` | 0 | 2.886437 | 4.294515 | 5.899288 |
| `rr` | 0 | 1.734681 | 1.975422 | 2.078264 |

The original 64-entry pair union saturated for absent two-letter terms whose individual letters occur in the corpus. Reused verification scratch had already removed candidate path allocations; adding a 128-bit pair filter per slot reduced those candidate verifications while preserving all semantics. The earlier slower raw run remains available alongside the final run.

| Short negative | Union-only p95 ms | Per-entry pair p95 ms |
| --- | ---: | ---: |
| `ii` | 9.935961 | 1.134889 |
| `ia` | 10.885420 | 4.294515 |
| `rr` | 10.304970 | 1.975422 |

The final immutable Data estimate is 10,002,153 bytes, 1,605,664 bytes above the earlier union-only version. The extra pair array costs 16 bytes per physical slot; directory caches and filters share snapshot segments and are charged before allocation. These counters describe conservative allocation capacity, not RSS. The full benchmark harness retained filesystem oracle/result vectors and optional sort output; its final VmRSS was 48,128 KiB and VmHWM was 54,904 KiB. That harness observation is not a production-process RSS gate. There were 2,001 actual native watches.

Validation: 31 focused native query/CLI/budget/checkpoint/compaction tests passed (one separately gated 100k checkpoint test stayed ignored). Linux all-target checking passed. Windows GNU all-target cross-checking passed, with existing source/test-helper dead-code warnings; it supplies compile evidence only. Public cancellation during execution, sorted snapshot retention, release unblocking publication, old lease directory moves, byte-cap failure without a partial count, and readable direct leases after stop are covered. The final driver also rejects a third concurrent query worker and cancels/joins admitted workers.

Session artifacts:

- Final raw rows: `/workspace/linux-ticket-12-query-100k.jsonl`
- Final case/aggregate/resource log: `/workspace/linux-ticket-12-query-100k.log`
- Complete summary: `/workspace/linux-ticket-12-query-100k-summary.json`
- Preserved slower union-only rows/log: `/workspace/linux-ticket-12-query-100k-block-pairs.jsonl` and `.log`
- Intermediate per-entry pair rows/log: `/workspace/linux-ticket-12-query-100k-pair-first.jsonl` and `.log`

These session artifacts are outside the repository; the tracked driver, table and source commit provide the reproducible method. No extrapolated million-entry pass is reported.

The later actual million-entry, 52-query worker phase and bounded wider pair
filter are recorded in [million-entry pair filter measurement](LINUX-MILLION-PAIR-FILTER.md).
It preserves each query/pass p95 and actual worker RSS/HWM, separately from this
100k baseline and from final unified-source/filesystem acceptance.
