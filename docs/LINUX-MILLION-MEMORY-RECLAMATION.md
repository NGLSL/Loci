# Million-entry lifecycle memory development evidence

The normal production run at source
`a7764c9979a9969963707d1904891dc991abce82` meets the 200 MiB settled RSS
and 512 MiB peak limits after compaction and each of three corrections.
This is Linux overlayfs development evidence. Reference SSD/NVMe, ext4/Btrfs,
the final-source 52-query/events/restart matrix and uninterrupted 600-second idle
measurement remain separate acceptance gates. Ticket14 is not resolved here.

## Actual process and corpus

The release public MonitorOwner ran at its production 20 ms cadence in worker
PID117696; the controller performed independent native-byte/kind oracle work
and /proc sampling in another process. The unchanged corpus is
`/workspace/loci-million-varied-20261002/data`: exactly 1,000,000 non-root
entries, including 20,000 directories. There were 20,001 actual kernel watches,
one inotify FD and seven process FDs at each settled cut. Every cut had
Validated status, the same version before/after its quiet observation, zero
leases, zero queued events and no compaction in progress.

| Settled stage | Actual observation | RSS MiB | HWM MiB | CPU % of one core |
| --- | ---: | ---: | ---: | ---: |
| Compaction | 5 s | 166.641 | 357.168 | 0.1975 |
| Correction0 | 5 s | 166.922 | 357.168 | 0.7849 |
| Correction1 | 5 s | 176.109 | 357.168 | 0.3949 |
| Correction2 | 10 s | 181.961 | 357.168 | 0.2979 |

The JSONL retains exact durations and raw /proc bytes/ticks. The configured
`--idle-seconds 600` option is not executed by `--phase stages`; these short
settles do not constitute a 600-second idle pass. Initial RSS was166.832 MiB.
The maximum actual HWM including save was374,517,760 bytes (357.168 MiB).
Same-PID stop returned three FDs, zero watches and zero inotify FDs; its
independently sampled RSS was10,661,888 bytes.

All complete byte and kind sets matched at initial, renamed/restored subtree,
compaction and each correction. A held version1 lease retained the original
million paths/kinds during rename. Query cancellation was observed, correction
cancellation retained its previous queryable cut, and the final save completed.
This phase does not time or verify all52 query cases.

Raw output: `/workspace/linux-million-memory-compactkeys-a7764c9/results`.
Executable SHA256:
`e4b0442414236bf77f00667c1fa2c1b24f4814373fae73d3d6bf5eb8cc9cf811`.
The unchanged52-query file SHA256 is
`53b6379f77d0fb79c134285b92382347a2c029d598f5278ad2d011eae4b99a74`.
The initial oracle byte-set hash is
`bb6d5f09e9c61b2d151948d2ab814019ecea171749eb3ae3cc335294bfb65c9d`;
its typed kind-set hash is
`77a87b3b2253aa597602fe77df14681a6728124e02a26e57e02349867d740235`.
Manifest, original timings/resources JSONL, final canonical sets and per-stage
comparison receipts remain in the owned output. Subsequent delivery commits
merge tools/docs and document the result; they are not the measured source SHA.

## Failure evidence and allocator diagnosis

Every failed run remains intact; a later passing stage cannot hide an earlier
failed cut. These are settled, unleased epochs, not maintenance peak readings.

| Source/run prefix | Compaction RSS MiB | Correction0 | Correction1 | Correction2 |
| --- | ---: | ---: | ---: | ---: |
| Original16a617d | 324.43 | 317.50 | 334.90 | 371.55 |
| GNU trim only cce7ec1 | 194.23 | 223.94 | 222.57 | 228.15 |
| Mapped buffers459153d | 197.03 | 195.99 | 198.82 | 208.24 |
| Packed child lists f4ff0cd | 185.41 | 221.49 | 186.21 | 186.05 |
| Packed prefixes c8e035f | 192.52 | 186.97 | 210.18 | 187.00 |
| Compact writer keys a7764c9 | 166.64 | 166.92 | 176.11 | 181.96 |

Original evidence is at `/workspace/linux-ticket-14-stages-16a617d/results`.
Subsequent preserved directories under `/workspace/` are:

- `linux-million-memory-diagnostic-46e62f8/results` (read-only GNU diagnosis).
- `linux-million-memory-reclaimed-cce7ec1/results`.
- `linux-million-memory-mapped-459153d/results`.
- `linux-million-memory-packed-f4ff0cd/results`.
- `linux-million-memory-prefix-c8e035f/results`.
- `linux-million-memory-manual-9cc8d77/results` (manual diagnostic only).

In the original-layout46e62f8 diagnostic, GNU in-use arena chunks plus GNU mmap
chunks stayed approximately191 million bytes, while free arena chunks rose from
5.37 to200.01 million bytes after maintenance. Published/retained inventory
capacity stayed stable, queues were empty and actual watch counts were stable.
These observations support allocator page retention rather than an expanding
live graph/native queue. GNU chunk statistics include allocator bookkeeping and
are neither RSS nor live Rust payload.

Diagnostic source`9cc8d774a03216da00b213d6f1619b1962d77550` repeated a manual
native malloc_trim only after each normally logged unleased settle. At its failed
correction1 cut, RSS changed209.56→209.49 MiB with zero remaining jobs/leases;
another trim did not recover the pinned pages. That entire run is diagnostic,
never production GREEN. The temporary manual opcode/environment probe was
removed before sourcea7764c9 and is absent from the delivered driver. Original
normal-cut and manual before/after rows remain separate in its raw JSONL.

## Layout, admission and validation

Large raw records, basename arenas and filters now use scale-private anonymous
mappings. The last Arc drop unmaps them directly, while old leases retain their
own mappings. Page-rounded capacity, COW and transient name-arena growth are
charged before allocation. GNU trim is deferred after an actual large release
and drained outside shared snapshot locks; quiet polls check an atomic flag.
Small child lists and directory prefixes avoid separate persistent allocations,
with Vec/shared-vector fallback for larger values. Writer lookup keys preserve
all96 parent/hash bits in12 bytes; sibling positions use checked u32 conversion
under the existing entry-ID/live-entry bounds. No corpus or file-path cache is
introduced. Platform and ownership details are in
[ADR0001](adr/0001-linux-scale-entry-inventory.md#scale-buffer-layout-and-page-reclamation).

The final correction's accounted snapshot/retained capacities are121,528,448 /
121,544,184 bytes. Conservative process admission reserved1,847,613,504 bytes
against its shared4 GiB ceiling. These are distinct from the measured181.961 MiB
RSS. The two-writer adjacency reservation increased to2048 bytes per directory
for inline bucket capacities. Kernel slab bytes remain unavailable and are not
inferred from RSS or watch counts.

41 focused public tests passed: query jobs10, budgets5, checkpoints9,
compaction9 and recovery8; the opt-in100k checkpoint test remained ignored.
Formatting and all-target checking passed. The added public case covers
>128-byte ancestry, raw invalid bytes, Unicode extension matching, moves to
short ancestry and back, held old leases and checkpoint restore. No full suite
or acceptance review was run for this development fix.
