# 12: 短词与高匹配查询先返回首屏，完整计数另算

Task: LOCI-LINUX-001-12
Type: task
Parent: LOCI-LINUX-001
Publication: published — 用户已确认粒度和依赖
Status: resolved

**What to build:** 用户输入短词、常见词或路径条件时，首 50 条结果及时出现，完整数量与可选全局排序不会阻塞首屏。

**Blocked by:** 11 — 监控所有者持续运行，查询可取消且能有界关闭

**Acceptance criteria:**

- [ ] 首屏、精确计数和可选全局排序有分离的公开完成/取消状态，固定快照分页顺序保持一致。
- [ ] 1/2/3 字符、中文、扩展名、名称/路径、无命中和高命中查询都与完整 oracle 无漏项；不为快而改变匹配语义。
- [ ] 跨段/祖先路径匹配、目录 rename 后的缓存失效正确，首屏不需先生成所有完整路径或排序全部匹配。
- [ ] 执行中取消和慢读者有界；共享模式/原有 Windows 查询语义不被无意改变。
- [ ] 在 100k 实例建立可复现的 p50/p95/p99 与选择性基线，向百万级 p95 ≤ 50 ms 目标优化；未达标不能报通过。
- [ ] CLI 可先展示结果再报告完整计数，用户能取消后续工作。

**Testing boundary:** 公开查询作业/分页/取消与 CLI；精确 oracle 与隔离原生计时。

**User stories covered:** 5, 6, 7, 10, 11, 12, 13, 28

## Comments

- 2026-10-02：依据既有 Linux 规格，按 to-tickets 准备独立本地 ticket 草稿；确认后 Status 改为 ready-for-agent 并逐票发布。父规格和既有 01 任务未修改。


- 2026-10-02：用户批准“按现有拆分发布本地 issues”，随后授权按依赖开发并统一 review。

- 2026-10-02：11已整合b1fc632；按独立工作树开始本票实现，最终统一review不逐票review。

## Answer

已在 a66ccfb 实现 snapshot-owned trigram/pair/block filters、目录normalized ancestry缓存、独立 count/sort QueryJob与CLI count/sort/cancel；0437787融合13共享容量策略，测量源码/driver f3986dd，最终文档 b4c77f2。原始bytes/invalid UTF-8边界/Unicode/AND/ext/祖先/slash/目录rename/旧租约均保持语义；首屏按entry-ID，完整排序保留bounded IDs+merge scratch。process queryworker cap2；sort默认16MiB，额外512KiB scratch纳入共享process admission；取消/stop不发布不完整精确total。

31项焦点检查通过；Linux和Windows GNU all-targets类型检查通过（GNU不是Windows原生）。同一真实100k实例：39查询各200次，共7800原始timing，39完整分页oracle、39精确count、完整100k raw排序全部正确。最终每类最差dir00000 p50/p95/p99=4.331277/4.779512/5.070162ms；aggregate另报。短negative ii/ia/rr优化前最差10.885420ms，最终1.134889/4.294515/1.975422ms。Data capacity10,002,153B；harness RSS48,128KiB/HWM54,904KiB含oracle/sort，不作为production百万RSS。

证据：docs/LINUX-SCALE-QUERY.md；/workspace/linux-ticket-12-query-100k-summary.json；/workspace/linux-ticket-12-query-100k.jsonl（最终）；block-pairs/pair-first对应JSONL+log保留优化前/中间测量。merger在f3986dd复验27项query/compaction/owner/CLI全部通过，fmt/alltargets通过。最终ext4/Btrfs/百万/Windows-native/24h门槛由14–18验收，不凭本票称完成。
