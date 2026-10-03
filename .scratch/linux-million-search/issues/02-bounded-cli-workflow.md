# 02: 用现有引擎跑通真实目录 CLI 闭环

Task: LOCI-LINUX-001-02
Type: task
Parent: LOCI-LINUX-001
Publication: published — 用户已确认粒度和依赖
Status: resolved

**What to build:** 用户显式选择一个真实目录后，可以建库、持续监听、查询、查看状态和请求重建，观察实际文件变化；这一票先保持现有有界规模。

**Blocked by:** 01 — 原生正确性修复（已完成）

**Acceptance criteria:**

- [x] 提供真实目录的 build/watch/query/status/rebuild 流程，复用公开 Engine，保留原实验命令而不混淆合成数据和用户数据。
- [x] CLI 展示版本、完整性、校正/失败/停止状态；不把 Pending 旧结果称为最新。
- [x] 数据库与其临时文件位于选定根之外，结果输出与诊断分开；参数错误返回明确退出状态。
- [x] 在工程测试根通过 CLI 验证增删改名、保存和重开；中文、空格、AND 子串及扩展名条件与公开引擎一致。
- [x] 仅扫描显式范围，不安装系统服务、不创建大规模夹具；当前 4096 条限制在帮助和错误中明确。

**Testing boundary:** 公开 Engine 与真实 CLI 子进程；小型原生文件夹具。

**User stories covered:** 1, 4, 5, 6, 7, 29, 30

## Comments

- 2026-10-02：依据既有 Linux 规格，按 to-tickets 准备独立本地 ticket 草稿；确认后 Status 改为 ready-for-agent 并逐票发布。父规格和既有 01 任务未修改。


- 2026-10-02：用户批准“按现有拆分发布本地 issues”，随后授权按依赖开发并统一 review。

- 2026-10-02：已认领；在 codex/linux-ticket-02 独立工作树实现，集成到 codex/linux-million-search。

## Answer

已在 cfdbe50 实现真实 Engine CLI build/watch/query/status/rebuild；原生 CLI 4/4 通过，Linux/Windows GNU all-targets 类型检查通过。诊断 stderr 与可逆输出分离，当前4096条限制明确。
