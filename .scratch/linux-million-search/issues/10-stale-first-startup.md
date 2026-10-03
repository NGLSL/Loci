# 10: 重启先提供旧索引，再校正离线变化

Task: LOCI-LINUX-001-10
Type: task
Parent: LOCI-LINUX-001
Publication: published — 用户已确认粒度和依赖
Status: resolved

**What to build:** 重启时用户尽快搜索已保存快照，随后看到后台校正进度与离线增删改名的最终结果。

**Blocked by:** 08 — 规模库存可保存并在新进程重开；09 — 漏事件与覆盖变化后有界校正并保留旧查询

**Acceptance criteria:**

- [x] 先检查根/来源/范围身份并安装监控；有效检查点在完整校正前可查询，状态明确为旧快照/Pending。
- [x] 离线 add/delete/rename 和启动扫描中的变化被恢复，完整集合与独立 oracle 一致。
- [x] 公开加载、查询和分批校正接口不以完成整根扫描作为首次可搜索的前提；不假装 Linux 有持久 USN 日志。
- [x] 校正失败、取消和 root/source 不匹配明确可见，保留可恢复文件；不存在将旧库判为最新的路径。
- [x] CLI 可演示旧结果→校正中→可靠结果；分别计量首次可搜索与全部校正时间。

**Testing boundary:** 公开重开与查询，跨进程离线变更和启动竞态；CLI 状态输出。

**User stories covered:** 4, 23, 24, 25, 26, 29

## Comments

- 2026-10-02：依据既有 Linux 规格，按 to-tickets 准备独立本地 ticket 草稿；确认后 Status 改为 ready-for-agent 并逐票发布。父规格和既有 01 任务未修改。


- 2026-10-02：用户批准“按现有拆分发布本地 issues”，随后授权按依赖开发并统一 review。

- 2026-10-02：08/09已整合75ba93c，开始旧快照先可搜索、后台校正和CLI stale/fresh状态实施。

## Answer

ee2e9f3 已合入。身份/范围检查及root watch完成后，已保存快照在首次扫描前以原版本Pending可查询；离线add/delete/目录rename与启动扫描中真实变化经完整oracle核对。取消/校正失败保留旧查询和检查点，source/root mismatch拒绝误用。CLI --fresh、stale→correcting→validated流程与 first_searchable_ms/full_correction_ms分别计量。三项实际red→green feature验证，29项实现阶段相关测试通过，合入24项复验通过；Linux全目标和Windows GNU类型检查通过。此为开发行为验证，不宣称百万启动性能或原生Windows门槛已完成。
