# 13: 增量合并和事件风暴不影响可查询性，空闲开销低

Task: LOCI-LINUX-001-13
Type: task
Parent: LOCI-LINUX-001
Publication: published — 用户已确认粒度和依赖
Status: resolved

**What to build:** 持续变化和后台合并时用户仍能查询；文件停止变化后引擎不会频繁全盘扫描或持续占用 CPU。

**Blocked by:** 11 — 监控所有者持续运行，查询可取消且能有界关闭

**Acceptance criteria:**

- [ ] 增量段/删除标记/背景合并按预算原子发布，过程中所有租约得到一致版本。
- [ ] 普通事件、风暴超限和读者背压的行为明确；队列、增量数据和两代共存峰值不会持续无界增长。
- [ ] 合并前后及保存/重开后完整集合一致，取消/失败保留最后有效状态；真实目录 rename 路径仍正确。
- [ ] 十分钟静默窗口记录审计活动与 CPU，面向单核平均 ≤1% 的规范目标；不能隐藏一次整根重扫成本。
- [ ] 实测变化可见延迟包含生产冷却/轮询，普通 add/delete/rename 面向 p95 ≤500 ms；虚拟时钟数值单列。
- [ ] CLI 实例可展示查询与合并并行、暂停/恢复，以及关闭后的资源回到基线。

**Testing boundary:** 公开运行实例、完整查询和生命周期；原生风暴/慢读者/静默计量。

**User stories covered:** 14, 15, 21, 27, 28, 31

## Comments

- 2026-10-02：依据既有 Linux 规格，按 to-tickets 准备独立本地 ticket 草稿；确认后 Status 改为 ready-for-agent 并逐票发布。父规格和既有 01 任务未修改。


- 2026-10-02：用户批准“按现有拆分发布本地 issues”，随后授权按依赖开发并统一 review。

- 2026-10-02：11已整合b1fc632；按独立工作树开始本票实现，最终统一review不逐票review。

- 2026-10-02 21:41 UTC：57cca5b已整合（基于b4c77f2）。原f3986dd静默窗口由root中止，实际365.6s单核CPU1.012%，raw/interrupted.json完整保留，不作为600s通过。公开real owner回归复现scan_batch1 compaction期间普通新增超过500ms；模拟可信连续batch回归复现full_scans从1增到2；两处已修，空nativecapture保留loss/rename/deadline逻辑后跳过全watchmap克隆。36项implementer焦点、30项merger焦点通过，真实overflow保留。新exact57cca5b actual200/class100k事件+准确1M全byteoracle/20001watches后单个完整600s quiet gate启动；本票尚未resolved。

## Answer

核心18af2b7、修复57cca5b，最终文档8d43763（含root5fceb8f tools）。提供按min(scan_batch,4096)预算的parent-first compaction、新epoch/原子发布/旧lease/取消/reader背压；queue/name/slot/两代Data/workers/checkpoint等共享4GiB conservative capacity admission。额度在native/source/candidate/transient分配前申请，查询状态/租约保留owner信用；reserved capacity不是RSS或内核字节。普通事件优先且可信连续batch不触发fullroot scan，empty nativepoll不克隆完整watch表。

真实测量源码57cca5b；独立EnginePID86293，实际1,000,000非root条目与完整原始路径oracle一致，20,001actualfdinfo watches。完整600.087612s窗口：单逻辑CPU0.281626%，RSS176,570,368B（168.39MiB）稳定，HWM192,757,760B（183.83MiB），actualfds5。full_scans仍1/scanned_entries1M/version1；审计1920目录、scopechecks+597保持活动。stop actualwatches0/fds3恢复基线。100k真实production20ms owner各20warmup+200timed：add/delete/rename p95 22.563/22.383/22.366ms；events/compaction完整byteoracle正确，回收660slots/48620名称bytes，full_scans不增。

36项implementer焦点、30项merger焦点全部通过；保留真实kerneloverflow与smallnative/simulated来源标签。先前f3986dd365.6s静默partial已中止且单核CPU1.012%，日志保留不计完成。新sampler legacy run_id误标100k已附erratum，原始raw未改；actualPID/root/1Moracle/20001watches证明实际1M。证据：docs/LINUX-SCALE-COMPACTION-VALIDATION.md；/workspace/linux-ticket-13-measurement-57cca5b/summary.json+manifest/raw/sources。该overlay开发证明不是最终SSD/ext4/Btrfs、1M普通churn或24h验收；14–18继续。
