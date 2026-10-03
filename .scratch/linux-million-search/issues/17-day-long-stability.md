# 17: 连续运行 24 小时保持结果和资源稳定

Task: LOCI-LINUX-001-17
Type: task
Parent: LOCI-LINUX-001
Publication: published — 用户已确认粒度和依赖
Status: ready-for-agent

**What to build:** 用户长期保持 Linux 搜索开启时，持续变化、静默、重启和校正不会让结果漂移或资源持续泄漏。

**Blocked by:** 15 — ext4 上完成规模模式原生行为验收；16 — Btrfs 上完成规模模式原生行为验收

**Acceptance criteria:**

- [ ] 真实 ext4 规模实例完成 24 小时墙钟运行，包括静默、受控 churn、风暴、慢读者、权限变化和重启。
- [ ] 多个稳定截止点用完整 oracle 核对；Pending/Failed/Stopped 不冒充已校正，恢复时间有记录。
- [ ] RSS、CPU、队列、watch/fd、增量段/日志增长曲线有界，无持续单调泄漏，退出回到资源基线。
- [ ] 长任务有专用根、磁盘/inode/时间/输出预算和可恢复记录；在 issue Comments 记录运行身份与最终结果。
- [ ] 实现可复现有界长跑入口；未完成真实 24 小时不能仅凭脚本创建或短时模拟将任务标 resolved。

**Testing boundary:** 长期公开 Engine/CLI 运行，独立 oracle、实际资源曲线与重启。

**User stories covered:** 14, 15, 19, 20, 21, 23, 25, 27, 28, 31, 33

## Comments

- 2026-10-02：依据既有 Linux 规格，按 to-tickets 准备独立本地 ticket 草稿；确认后 Status 改为 ready-for-agent 并逐票发布。父规格和既有 01 任务未修改。


- 2026-10-02：用户批准“按现有拆分发布本地 issues”，随后授权按依赖开发并统一 review。
