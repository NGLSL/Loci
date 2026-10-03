# 06: 在两千目录中持续观察并报告覆盖缺口

Task: LOCI-LINUX-001-06
Type: task
Parent: LOCI-LINUX-001
Publication: published — 用户已确认粒度和依赖
Status: resolved

**What to build:** 用户选择目录较多的树后，新增子目录能被及时监控；权限或监听预算不足时明确知道哪些范围未覆盖。

**Blocked by:** 04 — 用目录项身份完成搜索与目录改名闭环

**Acceptance criteria:**

- [x] 支持至少 2,000 目录的显式规模模式预算，保留旧模式固定限制，不仅删除 128-watch 常量。
- [x] 每目录先安装 watch 再枚举；初始扫描与 move-in/new-directory 期间的子项变化不被遗漏。
- [x] 每会话与进程共享的 watch/fd/队列预算可观察；系统 ENOSPC、应用超限和权限错误报告具体未覆盖范围。
- [x] 无静默丢监听、自动 sysctl 修改或无界重试；缺口存在时不发布完整 Validated。
- [x] 真实新增/退休子树、部分打开失败、stop/drop 与 root 身份失效后资源回到基线。
- [x] 小型 CLI 演示状态；较大夹具先核对磁盘/inode/时间预算，仅在专用根创建。

**Testing boundary:** 公开 Engine 的范围/状态/查询，原生 inotify 与实际 fd/watch 计数。

**User stories covered:** 4, 14, 16, 19, 22, 27, 28

## Comments

- 2026-10-02：依据既有 Linux 规格，按 to-tickets 准备独立本地 ticket 草稿；确认后 Status 改为 ready-for-agent 并逐票发布。父规格和既有 01 任务未修改。


- 2026-10-02：用户批准“按现有拆分发布本地 issues”，随后授权按依赖开发并统一 review。

- 2026-10-02：04已合入60d9db0，开始本票独立实现。

## Answer

e28722e 已实现共享实际监听预算、反向路径查找、明确覆盖缺口/资源报告、有限重试；4项新原生监听测试通过（2000目录、move-in、普通用户权限恢复、停止释放），旧模式限制保留。实际内核ENOSPC未强制触发，错误分类已实现；不将其称为原生ENOSPC验收。

两票在613d0df整合；格式/全目标检查及23项相应整合测试通过。
