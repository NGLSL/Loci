# Linux 百万级文件搜索引擎与 CLI 基础

Task: LOCI-LINUX-001
Type: feature
Status: ready-for-agent
Created: 2026-10-02（America/Los_Angeles）
Baseline: e204917（origin/main；当前云工作树 work）
Scope: Linux 产品化第一阶段；Everything 级别双平台产品的 Linux 引擎基础
Verification boundary: 已由用户确认，以公开 Engine 行为为主，CLI 验证用户流程，真实文件操作验证平台行为
Prerequisite: issues/01-native-engine-regressions.md；完成原生正确性修复后再改规模架构

## Problem Statement

用户要做能够日常使用的 Everything 级别 Windows/Linux 文件名与路径搜索产品。Linux 已有可运行的原生引擎，但现在最多 4096 条库存、128 目录、1 MiB UTF-8 路径输入，不能用作普通电脑的百万级文件搜索工具。静态百万条合成记录测试不代表百万真实文件的建库、持续监听、更新、重启和资源占用已经达标。

当前 Linux 原生装配已经完成，不需要重新实现这一里程碑。最新主分支文档记录：同一代码验收提交在 Linux/ext4 上 debug/release 各 119 passed，在 Windows/NTFS 上各 96 passed。这是文档/CI 的既有证据，不是本任务的新运行结果，也不是规模化验收。

独立验证分支 `validation/linux-engine-regressions-20261002` 在 `104b218` 增加两项 expected-red 原生回归：排除目录替换已索引空目录后假 Validated/漏子项，以及数据库父路径改变后的保存重定向。它们应作为本规格的前置正确性任务，而非等规模化后再补；既有 119 项通过不覆盖这两种情况。

用户需要首次选定目录后快速建库，输入关键词立即看到结果，文件增删改名后持续更新，重启后尽快使用旧索引并明确当前校正状态，同时保持低空闲开销。Linux 无通用 USN 式离线变化日志，不能把离线库存冒充当前目录的最新完整结果。

## Solution

在保留现有查询语义、异常恢复行为和 Windows 兼容性的前提下，提供可支撑百万真实目录项的 Linux 引擎与最小用户 CLI。先支持一个显式选择的根目录及其本地文件系统，不默认扫描整台机器，不默认跨挂载点。

引擎提供批量建库、持续监听、查询首屏、完整结果枚举、保存、重开、取消和停止。规模化库存采用对象身份与目录项/父子关系，避免每次少量文件变化都复制完整库存或重建完整索引。持久库允许快速提供可搜索快照，随后在后台校正；用户始终能区分旧结果、完整性、覆盖缺口和监控状态。

先用 10 万真实目录项完成结构和增量正确性，再用 100 万真实目录项验收产品目标；千万级压力作为独立实验，不以其通过代替真实文件验收。ext4 与 Btrfs 的能力和测量分别报告。此任务交付 Linux 搜索产品的引擎基础与 CLI，完整桌面 GUI 是后续独立任务。

## User Stories

1. As a Linux user, I want to choose an indexing root explicitly, so that I control which files are indexed.
2. As a Linux user, I want mount boundaries to be respected, so that indexing does not unexpectedly traverse other volumes.
3. As a Linux user, I want one million real directory entries to be searchable, so that I can index a normal workstation without prototype limits.
4. As a Linux user, I want indexing progress and coverage errors to be visible, so that I know when results are incomplete.
5. As a Linux user, I want filename and full-path substring search, so that I can find files from either their names or their locations.
6. As a Linux user, I want case-insensitive AND terms and extension filters, so that existing searches remain predictable.
7. As a Linux user, I want Chinese, spaces and punctuation to work, so that my everyday filenames are searchable.
8. As a Linux user, I want non-UTF-8 filenames to retain their original bytes, so that one unusual filename does not invalidate my whole index.
9. As a Linux user, I want valid text portions of unusual filenames to remain searchable, so that those entries are discoverable without corrupting their names.
10. As a Linux user, I want quick first-page results for short and common queries, so that typing remains responsive.
11. As a Linux user, I want complete results to be paginated or exported from one snapshot, so that I can inspect more than fifty matches reliably.
12. As a Linux user, I want result order to be documented and stable within a snapshot, so that pagination does not omit or duplicate entries.
13. As a Linux user, I want expensive counts and global sorting to avoid blocking the first page, so that broad searches remain useful.
14. As a Linux user, I want newly created and deleted files to appear and disappear promptly, so that results follow my work.
15. As a Linux user, I want file and directory renames to update paths correctly, so that results do not retain old names or lose descendants.
16. As a Linux user, I want moved-in directories to receive watches before enumeration, so that early child changes are not missed.
17. As a Linux user, I want hard links to appear as distinct searchable paths, so that all directory entries remain discoverable.
18. As a Linux user, I want symbolic links to be listed without automatically following them, so that indexing avoids cycles and unintended traversal.
19. As a Linux user, I want permission failures and watch-budget exhaustion to identify affected scope, so that partial coverage is never presented as complete.
20. As a Linux user, I want permission restoration to permit recovery, so that a temporary failure does not require deleting my database.
21. As a Linux user, I want lost or overflowed events to trigger bounded correction, so that the index eventually becomes accurate again.
22. As a Linux user, I want root replacement and unmount to invalidate the affected source, so that results are not attributed to a different filesystem.
23. As a Linux user, I want saved results to become searchable quickly after restart, so that I do not wait for a full rescan before every search.
24. As a Linux user, I want stale startup results to be labelled until correction finishes, so that I understand their reliability.
25. As a Linux user, I want changes made while the program was stopped to be reconciled, so that the saved index does not silently stay out of date.
26. As a Linux user, I want damaged or incompatible databases to fail clearly while preserving recoverable data, so that recovery is deliberate.
27. As a Linux user, I want resource use and queued work to stay bounded during event storms, so that indexing does not exhaust my computer.
28. As a Linux user, I want cancellation and shutdown to finish promptly and release watches and descriptors, so that the program is safe to close.
29. As a Linux user, I want a CLI to create an index, watch, query, inspect status and request rebuilding, so that I can use the engine before the desktop UI is ready.
30. As a Linux user, I want output paths to preserve exact filesystem identity, so that scripts can act on results without shell interpretation or lossy decoding.
31. As a Linux user, I want low CPU use when nothing changes, so that keeping the search engine running has little daily cost.
32. As a product maintainer, I want Linux shared-core changes to retain Windows behavior, so that the two platforms can advance independently.
33. As a product maintainer, I want reproducible real-filesystem correctness and performance reports, so that readiness is based on evidence.

## Implementation Decisions

### Public behavior and compatibility

- Use the existing Engine lifecycle and query-handle concept as the primary public boundary. Preserve opening, monitoring, saving, stopping, immutable leases and query cancellation semantics. Introduce scale options and pagination/export at this boundary rather than adding many private test hooks.
- Preserve case-insensitive AND substring matching over name/path and exact extension filtering for valid text. Complete means traversal completeness; Validated refers only to the last reliable observed filesystem cutoff. These must remain separate.
- A query page and its cursor belong to one immutable snapshot. Specify default ordering, page limits, cursor expiration and cancellation. Invalidated or expired cursors return an explicit outcome. Exact count and optional global sorting may finish asynchronously.
- Keep the bounded v0.1 implementation available as a compatibility/control path while scale mode is developed. Do not silently reinterpret the old database format or remove its resource limits.
- Native Linux scope starts with x86_64, explicit local roots and no implicit mount traversal. Detect source identity changes and preserve the reason in status. A composite view across multiple roots is a later extension, not this task's completion gate.

### Inventory and query scale

- Separate filesystem object identity from searchable directory entries. Object identity includes source/mount epoch and available platform identity; do not assume inode numbers alone or universally available inode-generation fields are sufficient. Hard links can share an object while keeping distinct entries.
- Store original name bytes and parent relationships independently from the search representation. Resolve paths from a consistent snapshot. Directory renames must not require duplicating all full paths as the primary update, but ancestor-query invalidation and sorting may still require subtree work; no unconditional O(1) claim.
- Preserve non-UTF-8 bytes for lookup/export. Display invalid bytes with an unambiguous escaped representation; do not persist replacement characters as the original name. Valid UTF-8 portions use the existing lowercase semantics, without matching across an invalid-byte boundary. Raw-byte output must not pass through shell interpolation.
- Replace O(n) whole-inventory work on ordinary file changes with a segmented inventory/index and bounded delta publication. Bound reader retention, queued changes and background compaction by explicit budgets. Choice of mmap, segment sizing and posting compression follows measured baselines, not a requirement to use a specific library.
- Index symbolic links as entries without following their targets; distinguish their type from files/directories. Exclusions become explicit user configuration rather than silently inheriting every experimental build-directory exclusion.

### Linux observation, correction and resources

- Install each directory watch before enumerating its contents, including new/moved-in subtrees. Maintain paired rename correctness and conservative correction for ambiguous batches.
- Expose configurable per-engine limits and a process-wide allocator for actual watches/descriptors/memory. Kernel ENOSPC, permission errors or application limits create explicit coverage gaps; no silent drop, no automatic sysctl change, no unbounded retries.
- Preserve source/root identity checking, real IN_Q_OVERFLOW handling, old-query retention on failure, lease backpressure and stop/drop release. Register/unregister changes transactionally enough that a failed update cannot publish false complete coverage.
- Treat watch loss, mount changes and unknown event identity as a correction/failure condition with affected scope. Permission revocation must invalidate affected coverage; restoration permits bounded retry or explicit rebuilding.
- Replace the prototype's fixed thirty-second full-root audit in scale mode with budgeted coverage-aware auditing/correction. Reliable ordinary updates must not trigger full-root scans. Quiet full-scale idle operation must not depend on frequent whole-root rescans.
- Linux has no generic durable event cursor for offline periods. Reopening attaches monitoring and reconciles from disk; it may expose a loaded immutable snapshot as stale after root identity/scope checks, then correct it in the background. Do not claim it is current merely because the database loaded.

### Persistence and user entry point

- Introduce a separately versioned scale persistence format. Store scope/source identity, original bytes, relationships and the checkpoint/delta publication boundary coherently. Torn/truncated records and incompatible versions produce documented recovery or explicit rebuild requirements.
- Saving uses temporary ownership, durable writes and atomic replacement/commit boundaries. Failed writes preserve recoverable prior state. Crash-restart tests are distinct from physical power-loss durability claims.
- Support an interruptible background monitoring owner so CLI callers do not accidentally stop updating by retaining only a query handle. Define ownership and shutdown; do not create a second platform-specific query engine.
- Provide CLI flows for build, watch, query, status and rebuild. CLI queries use the public engine boundary, support machine-readable output and exact raw paths, and keep diagnostics separate from results. Document exit/status behavior for incomplete and failed work.
- Scope/database/config choices are explicit; index output is excluded from indexing itself. Lock/serialize competing writers, reject source mismatch and keep queries usable against published snapshots during updates.
- Shared changes remain compatible with Windows. Cross-platform integration is performed on an exact commit; no automatic release or main merge is included in this task.

## Testing Decisions

### Primary seam and prior art

- The user confirmed the test boundary: public Engine behavior is primary, CLI exercises end-to-end use, and real file operations establish Linux correctness. A good test checks returned paths, snapshot/state, recovery and released resources rather than copying implementation branches.
- Reuse existing engine tests with a controllable external EventSource for deterministic generation/loss/error cases. Label these simulated cases; they cannot prove native event delivery or real kernel overflow.
- Reuse native Linux Engine fixtures for real recursive changes, rename, root identity, stop/drop, cross-process save/reopen and kernel overflow. Extend that style rather than relying on the old standalone incremental prototype.
- Reuse hostile-storage child-process tests for malformed lengths, incompatible versions and bounded recovery. Reuse the existing cross-platform CI structure for exact shared-core commits.
- Current query results keep only fifty paths even in complete mode. Full-set scale verification must use public snapshot-bound pagination/export; do not infer correctness from count/checksum or the first page alone.

### Correctness matrix

Compare complete path sets and entry kind/identity where relevant with an independent filesystem traversal at a stable cutoff. Preserve original bytes, hard-link multiplicity, exclusions and mount policy in the oracle. Test before and after save/reopen, compaction, correction and cancellation.

Include file/directory add/delete/rename, dependent rename batches, atomic-save replacement, move-in/out, new directories during initial scanning, non-UTF-8 names, Chinese, hard links, links and cycles, permissions, watch exhaustion, root replacement, mount loss, event storms, user overflow, actual kernel overflow, corrupted/truncated databases and writer contention. Stable snapshot paging must neither omit nor repeat entries while newer snapshots publish.

Run permission cases as an unprivileged user; root-only runs do not establish those results. Real mount/unmount and kernel-overflow cases require a capable native environment; injected equivalents are separate coverage, never a substitute for release claims.

### Acceptance gates

| Gate | Required result |
| --- | --- |
| Baseline | Record exact commit, existing Engine native behavior and query semantics; preserve existing Linux/Windows regression suites |
| Model | Demonstrate identities/entries, original bytes and snapshot-bound enumeration on small adversarial fixtures; record schema/interface rationale |
| 100k | Index at least 100,000 real directory entries distributed across at least 2,000 directories; verify full sets, localized updates, persistence, recovery and resource limits |
| 1M | Index at least 1,000,000 real entries across at least 20,000 directories; verify full sets and common-query first pages, real event visibility and restart behavior |
| Filesystems | Run the new scale path on ext4 and Btrfs separately; overlayfs is development evidence only, not a replacement for either |
| Long run | Complete a 24-hour run including quiet periods, bounded churn, correction, query retention and restart; no silent loss or monotonic descriptor/watch growth |
| CLI | A user can build, watch, query, inspect coverage and rebuild without writing Rust; native engine is the same implementation used in integration tests |
| Cross-platform | Shared-core changes pass appropriate Windows native and Linux checks on the exact integrated commit; report unsupported cases separately |

The long run and large fixtures are opt-in bounded jobs, not ordinary per-commit unit tests. Lack of a capable filesystem, disk/inode budget or Windows runner is an explicitly unverified gate; it does not justify claiming completion. Implementation can proceed independently.

### Performance acceptance and method

Reference class: x86_64 Linux, at least 4 logical CPUs, 16 GiB RAM and local SSD/NVMe. Record exact CPU, memory, kernel, filesystem, mount options, permissions and system watch limits; do not generalize a cloud overlayfs result. Fixture records use name lengths 8–80 bytes, mean relative path length no greater than 160 bytes, mixed ASCII/Chinese and varied directory depth. Include separate directory-heavy and high-match stress cases rather than concealing them in one average.

For 1M on each reference filesystem, target:

- Engine first fifty results for the documented common-query suite: p95 ≤ 50 ms.
- Ordinary isolated file add/delete/rename to query visibility, including real polling/debounce: p95 ≤ 500 ms.
- Loading a valid checkpoint to a searchable stale snapshot: p95 ≤ 2 s. Time until full current coverage is separately measured and is not included in this claim.
- Steady engine-process RSS ≤ 200 MiB on the reference fixture; peak ≤ 512 MiB. GUI is outside this task, but process/IPC overhead cannot be removed from engine-process accounting. Kernel watch/slab bytes are reported separately when measurable; unavailable byte measurements are labelled unavailable, with actual counts.
- Idle CPU ≤ 1% of one logical CPU averaged over a ten-minute quiet window. Report background audit activity in that window.

These are requirements for the new implementation, not already measured facts. Failure requires an explicit result and an optimization/design follow-up, not silently changing corpus or metric. Initial scan, complete count, optional global sorting, large subtree rename, reconciliation and directory-heavy worst cases have separately reported latency/work; this task does not promise a universal 50 ms bound on all operations.

Use at least 30 diverse queries with 1/2/3-character terms, Chinese, extensions, name/path scope, no match and high match; at least 200 timed repetitions per ordinary query/event class after documented warm-up. Report p50/p95/p99, match distribution and cancellation. Repeat representative runs, publish raw structured logs and separate hot/cold cache claims. Do not bypass production debounce with a virtual clock for native timing.

Small-case oracle and full correctness checks stay outside timed hot-query loops. Actual disk/inode/time/output budgets are checked before fixture creation; fixtures only occupy explicitly designated test roots and cleanup validates ownership. Default CI remains bounded. No automatic whole-machine scan, global software installation or kernel configuration change is required.

## Out of Scope

- Reimplementing the already assembled Linux v0.1 native Engine or reusing its test count as new work.
- Windows MFT/USN backend development; that is a separate local Windows task. Shared contracts and Windows non-regression remain in scope.
- Desktop GUI, system tray, installers, updater and final consumer release; CLI and engine lifecycle are in scope here.
- Full-text, semantic/AI search, pinyin, cloud sync, relevance ranking and enterprise access control.
- Implicit indexing across all mount points, multi-root aggregation, network filesystems and broad removable-device support.
- Non-x86_64 Linux, kernel modules, mandatory privileged services and automatic increases to system resource limits.
- A universal offline replay guarantee on Linux, a physical power-loss guarantee, or a million-entry real-time claim supported only by synthetic strings.
- Automatic GitHub issue creation, PR publication, main merge or release. The project issue tracker for this task is local Markdown.

## Further Notes

### Work sequence

1. Complete the native regression prerequisite, then establish the latest baseline, public-boundary behavior and reproducible scale workload; record model and format decisions before freezing a new shared API.
2. Implement the compact directory-entry model and public snapshot enumeration with small adversarial tests; retain the existing bounded control path.
3. Implement batch scans, configurable watch/resource allocation and localized update publication; pass the 100k gate.
4. Implement scale persistence, stale-first restart, bounded correction and background ownership; pass cross-process and crash-restart cases.
5. Complete minimal CLI flows, 1M benchmarks, ext4/Btrfs native gates, long run and exact-commit Windows non-regression.

Specification readiness means an agent can start implementation without another product interview. It does not mean every gate is currently satisfied or that new architecture/performance has been proven. This to-spec task publishes one feature specification; separate implementation tickets are not yet generated.

### Evidence and execution state

- Main baseline: e204917. Existing native engine acceptance code commit: 657412e, documented cross-platform run https://github.com/NGLSL/Loci/actions/runs/37045846370.
- Native prerequisite repaired in 0c4fb7d: local overlayfs Linux debug/release each 124 passed, 0 failed, 6 ignored; Windows GNU cross-type checks passed. This is not a new ext4/Btrfs or native Windows acceptance result; those integration gates remain explicit.
- Earlier discussion used a pre-pull baseline where Linux Engine assembly was pending. That assessment is superseded; this task targets the remaining scale and product-use gaps.
- Current state: scale specification published; no scale architecture/benchmark or Windows session has been started. The user's subsequent validation-branch request initiated the native correctness prerequisite on `codex/linux-engine-regressions-fix`; its execution and final evidence are in the linked local issue.
- Task/config records are local, ignored by project rules and not automatically shared with the Windows computer. Share an explicit copy when coordinating; do not assume a Git pull includes them.

## Comments

- 2026-10-02: 用户要求使用 to-spec 先创建 Linux 任务；未要求 GitHub issue 或现在开始实现。
- 2026-10-02: 用户确认：“符合，按引擎行为为主继续”。采用公开 Engine 为主要验收边界，CLI 为用户流程补充，原生文件系统夹具验证 Linux 平台事实。
- 2026-10-02: 用户要求一并检查/处理 `validation/linux-engine-regressions-20261002`；其两项缺陷已在最新主分支代码复现，纳入前置任务，保持本规格的规模目标不变。
