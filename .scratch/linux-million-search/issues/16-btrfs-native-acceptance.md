# 16: Btrfs 上完成规模模式原生行为验收

Task: LOCI-LINUX-001-16
Type: task
Parent: LOCI-LINUX-001
Publication: published — 用户已确认粒度和依赖
Status: ready-for-agent

**What to build:** Btrfs 用户能看到经过原生验证的搜索与恢复支持范围，子卷和挂载身份不导致假完整结果。

**Blocked by:** 14 — 百万真实目录项达到查询、重启和资源目标

**Acceptance criteria:**

- [ ] 记录精确集成 SHA、Btrfs 挂载/子卷语义、内核/权限与硬件；复验 100k/1M 数据，独立于 ext4 结论。
- [ ] 根/来源、嵌套挂载/子卷边界明确；硬链接、目录 rename、权限变化和原始 bytes 的完整集合正确。
- [ ] 实际监听/overflow、保存/恢复、失败替换、关闭释放在 Btrfs 上有原生结果；未执行项不能算通过。
- [ ] CLI 运行行为与覆盖状态契约一致；发现兼容差异要修复并补窄回归，不能修改总规格来假装符合。
- [ ] 程序 fsync/重命名结果与掉电、快照功能保证区分，本票不新增整个文件系统快照产品功能。

**Testing boundary:** 同一公开 Engine/CLI 在真实 Btrfs/子卷范围的原生验收。

**User stories covered:** 2, 19, 20, 21, 22, 26, 28, 33

## Comments

- 2026-10-02：依据既有 Linux 规格，按 to-tickets 准备独立本地 ticket 草稿；确认后 Status 改为 ready-for-agent 并逐票发布。父规格和既有 01 任务未修改。


- 2026-10-02：用户批准“按现有拆分发布本地 issues”，随后授权按依赖开发并统一 review。
