# 18: 精确提交跨平台回归并交付 Linux CLI Alpha 基础

Task: LOCI-LINUX-001-18
Type: task
Parent: LOCI-LINUX-001
Publication: published — 用户已确认粒度和依赖
Status: ready-for-agent

**What to build:** 用户和维护者得到一个明确版本的 Linux 引擎/CLI 基础，并能确认共享改动保留 Windows 既有行为。

**Blocked by:** 17 — 连续运行 24 小时保持结果和资源稳定

**Acceptance criteria:**

- [ ] 固定最终整合 SHA，Linux/ext4/Btrfs 相关门槛和 Windows 原生 debug/release 回归有同版本可核查结果。
- [ ] Windows 不回归既有查询/生命周期/持久化行为；交叉类型检查、模拟输入、历史 CI 不能冒充此次 Windows 原生运行。
- [ ] 原生/性能/长期结果与未支持范围一致，保留第 01 项修复与支持的保存并发边界。
- [ ] CLI 用户文档可完成索引、监控、查询、覆盖检查、取消、重建及关闭；不需编写 Rust。
- [ ] 最终产品化第一阶段门槛逐项核对；缺少环境或未达目标保持未完成，不以若干绿测试数量替代。
- [ ] 本票不实现 Windows MFT/USN、GUI/安装器，不自动合 main、发布 PR 或 release；桌面产品后续独立立项。

- [ ] 若此票改变生产行为，在新精确 SHA 重跑受影响的规模/文件系统/长期门槛，不复用旧提交日志声称已验收。

**Testing boundary:** 同一提交的 Linux/Windows 原生公开接口、CLI 流程与已有规模/长跑结果。

**User stories covered:** 29, 32, 33

## Comments

- 2026-10-02：依据既有 Linux 规格，按 to-tickets 准备独立本地 ticket 草稿；确认后 Status 改为 ready-for-agent 并逐票发布。父规格和既有 01 任务未修改。


- 2026-10-02：用户批准“按现有拆分发布本地 issues”，随后授权按依赖开发并统一 review。
