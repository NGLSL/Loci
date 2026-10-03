# 15: ext4 上完成规模模式原生行为验收

Task: LOCI-LINUX-001-15
Type: task
Parent: LOCI-LINUX-001
Publication: published — 用户已确认粒度和依赖
Status: ready-for-agent

**What to build:** ext4 用户可依赖经过真实文件系统验证的规模模式，而不是从 overlayfs 或模拟测试推断兼容性。

**Blocked by:** 14 — 百万真实目录项达到查询、重启和资源目标

**Acceptance criteria:**

- [ ] 记录精确集成 SHA、ext4 挂载选项、内核/硬件/权限和资源限额，以同一公开模式复验 100k/1M 门槛。
- [ ] 真实 permissions、hard links、rename、mount/root 生命周期、overflow、fd/watch 释放和跨进程保存/重启结果完整。
- [ ] 真实行为与模拟 loss/竞态明确分开，跳过或环境不支持项不计通过；普通用户执行权限测试。
- [ ] CLI 运行流程、错误与恢复契约在 ext4 上一致；必要兼容修复保留小型回归并不破坏既有模式。
- [ ] 报告 fsync/原子替换的实测范围，不把普通程序测试称为掉电耐久性。

**Testing boundary:** 同一公开 Engine/CLI 在真实 ext4 的可复现原生验收。

**User stories covered:** 2, 19, 20, 21, 22, 26, 28, 33

## Comments

- 2026-10-02：依据既有 Linux 规格，按 to-tickets 准备独立本地 ticket 草稿；确认后 Status 改为 ready-for-agent 并逐票发布。父规格和既有 01 任务未修改。


- 2026-10-02：用户批准“按现有拆分发布本地 issues”，随后授权按依赖开发并统一 review。
