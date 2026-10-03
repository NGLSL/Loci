# 07: 十万真实目录项建库、搜索与局部更新

Task: LOCI-LINUX-001-07
Type: task
Parent: LOCI-LINUX-001
Publication: published — 用户已确认粒度和依赖
Status: resolved

**What to build:** 用户可在内存会话里实时搜索至少十万真实目录项，少量文件变化不触发全根扫描或完整库存复制。

**Blocked by:** 05 — 搜索原始名称并遵守链接、排除与挂载范围；06 — 在两千目录中持续观察并报告覆盖缺口

**Acceptance criteria:**

- [x] 至少 100,000 真实目录项分布于至少 2,000 目录，通过分批建库后公开查询完整集合与独立遍历一致。
- [x] 同一模式中真实 add/delete/file rename 和普通 directory rename 后查询与后续监听正确；保持第 01 项修复的边界。
- [x] 普通少量变化的库存/索引发布是局部工作，不把扩大旧上限或 O(n) 全量复制算规模化实现。
- [x] 查询、事件队列、增量段、旧读者及发布峰值受明确预算约束；超限时保留可说明状态，不崩溃或假完整。
- [x] CLI 在该模式完整演示建库、监控、分页和状态；暂未支持的新格式保存明确报错，不能写成旧格式冒充兼容。
- [x] 给出按同一场景可复现的建库、更新、查询与资源基线，合成记录另报。

**Testing boundary:** 公开 Engine/CLI 贯穿真实 100k fixture；全集合 oracle，独立原生计时。

**User stories covered:** 3, 4, 5, 6, 10, 11, 14, 15, 17, 27, 33

## Comments

- 2026-10-02：依据既有 Linux 规格，按 to-tickets 准备独立本地 ticket 草稿；确认后 Status 改为 ready-for-agent 并逐票发布。父规格和既有 01 任务未修改。


- 2026-10-02：用户批准“按现有拆分发布本地 issues”，随后授权按依赖开发并统一 review。

- 2026-10-02：05/06已整合613d0df，开始真实100k规模库存和局部更新验证。

## Answer

de42368 完成真实100k/2000目录全路径oracle、原生增删改名/目录退休、局部分段工作和显式物理预算。release开发基线：建库0.297s、完整分页0.160s、2001watches、孤立变化1.8–4ms；含oracle测试进程RSS45/HWM62MiB，不冒充生产百万RSS门槛。24项相关测试、Linux/Windows GNU检查通过。每owner物理预算已实现；共享进程内存分配器留给13。详见docs/LINUX-SCALE-100K.md。
