# 03: 在固定快照上翻页与导出全部匹配

Task: LOCI-LINUX-001-03
Type: task
Parent: LOCI-LINUX-001
Publication: published — 用户已确认粒度和依赖
Status: resolved

**What to build:** 用户能查看超过 50 个结果，并从同一版本导出完整路径集合；后台变化不会造成跨页漏项或重复。

**Blocked by:** 02 — 用现有引擎跑通真实目录 CLI 闭环

**Acceptance criteria:**

- [x] 公开查询边界提供固定快照的分页/完整导出，定义默认稳定顺序、页大小、游标版本和失效行为。
- [x] 超过 50 条的真实夹具通过独立目录遍历核对完整结果集合，不仅比较 count/checksum。
- [x] 旧快照翻页时发布新版本，旧游标仍保持原版本结果，或明确返回已失效；不能悄悄切换版本。
- [x] 取消、读者租约和释放均有界；complete 与观察有效性保持区分。
- [x] CLI 可演示多页与全量导出，原 first50 查询行为保持兼容。

**Testing boundary:** 公开 Engine 查询/分页/导出与 CLI；原生夹具加确定性发布竞态。

**User stories covered:** 11, 12, 13, 28, 33

## Comments

- 2026-10-02：依据既有 Linux 规格，按 to-tickets 准备独立本地 ticket 草稿；确认后 Status 改为 ready-for-agent 并逐票发布。父规格和既有 01 任务未修改。


- 2026-10-02：用户批准“按现有拆分发布本地 issues”，随后授权按依赖开发并统一 review。

- 2026-10-02：02 已合入 cfdbe50，开始03固定快照分页与完整导出。

## Answer

7e32f77 实现固定快照公开分页与CLI全量导出；原生137项全集合/旧快照发布竞态、121项CLI导出验证通过。相关34项通过、1项既有忽略；Linux/Windows GNU all-targets检查通过。
