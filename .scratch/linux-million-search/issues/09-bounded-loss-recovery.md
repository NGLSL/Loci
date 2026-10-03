# 09: 漏事件与覆盖变化后有界校正并保留旧查询

Task: LOCI-LINUX-001-09
Type: task
Parent: LOCI-LINUX-001
Publication: published — 用户已确认粒度和依赖
Status: resolved

**What to build:** 事件风暴、监听丢失、权限变化和卸载使结果可靠性失效时，用户还能看到带状态的旧结果，并观察校正恢复。

**Blocked by:** 07 — 十万真实目录项建库、搜索与局部更新

**Acceptance criteria:**

- [x] 用户队列溢出及实际 IN_Q_OVERFLOW 后失效、旧查询保留、监控重建和最终完整集合正确。
- [x] 权限撤销/恢复、未知 watch、root 替换、挂载失效分别报告原因和范围；普通用户验证权限行为。
- [x] 扫描/监控重建缺口与持续写入使候选失效，直到可靠切面才 Validated；不能先假完整再周期修复。
- [x] 校正分批、可取消、有限重试并有资源预算；大量目录时查询和状态仍可用。
- [x] 替换规模模式固定 30 秒全根重扫策略，采用有界覆盖审计；可靠普通事件保持局部更新。
- [x] 模拟 loss 与原生 overflow/mount 证据分开；CLI 能演示失败、待校正和恢复。

**Testing boundary:** 公开 Engine 查询/状态/恢复；可控 EventSource 配合真实 Linux 权限、overflow、挂载夹具。

**User stories covered:** 4, 19, 20, 21, 22, 27, 28, 31

## Comments

- 2026-10-02：依据既有 Linux 规格，按 to-tickets 准备独立本地 ticket 草稿；确认后 Status 改为 ready-for-agent 并逐票发布。父规格和既有 01 任务未修改。


- 2026-10-02：用户批准“按现有拆分发布本地 issues”，随后授权按依赖开发并统一 review。

- 2026-10-02：07已合入de42368，开始本票独立开发。

## Answer

f3097a4 已合入。实际内核 IN_Q_OVERFLOW（queue16384）、普通 uid1000 权限撤销/恢复、模拟持续 loss 有限重试、取消/恢复、旋转目录覆盖审计和旧结果保留均已验证。实现 RecoveryOptions 资源/重试预算、独立累计 observed_losses，以及 CLI cancel/rebuild 语义。实现阶段50项相关测试通过；合入26项恢复/CLI/Engine/scope/watch复验通过，Linux/Windows GNU类型检查通过。原生与模拟证据分列于 docs/LINUX-SCALE-RECOVERY.md；GNU检查不是Windows原生验收。
