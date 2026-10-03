# 05: 搜索原始名称并遵守链接、排除与挂载范围

Task: LOCI-LINUX-001-05
Type: task
Parent: LOCI-LINUX-001
Publication: published — 用户已确认粒度和依赖
Status: resolved

**What to build:** 包含非 UTF-8 名称、硬/软链接和挂载边界的选定目录可以搜索；结果名称可逆，不会因为异常名称使整个库存失效。

**Blocked by:** 04 — 用目录项身份完成搜索与目录改名闭环

**Acceptance criteria:**

- [x] 原始 Linux 名称 bytes 与显示/规范化搜索分离；非法 bytes 以可逆形式显示，路径输出无损。
- [x] 合法 UTF-8 部分继续按既有 lowercase 查询，不能跨非法字节边界误匹配；中文、换行、空格和冒号有真实夹具。
- [x] 符号链接作为独立类型返回，不跟随目标或形成循环；链接目标不在范围时不被偷偷索引。
- [x] 用户明确配置排除策略，索引自身输出仍被排除；不静默沿用所有实验目录的排除名称。
- [x] 根与嵌套挂载范围有明确策略，默认不跨挂载点；无法确定覆盖时报告原因，不能假完整。
- [x] CLI 导出采用 NUL 分隔或另一种可逆编码；不经 shell 插值解释路径。

**Testing boundary:** 公开查询/完整导出和 CLI；真实 raw-byte/link 夹具，挂载用能力允许的原生环境。

**User stories covered:** 1, 2, 7, 8, 9, 18, 30

## Comments

- 2026-10-02：依据既有 Linux 规格，按 to-tickets 准备独立本地 ticket 草稿；确认后 Status 改为 ready-for-agent 并逐票发布。父规格和既有 01 任务未修改。


- 2026-10-02：用户批准“按现有拆分发布本地 issues”，随后授权按依赖开发并统一 review。

- 2026-10-02：04已合入60d9db0，开始本票独立实现。

## Answer

f90febc 已实现原始名称/合法文本匹配/链接类型/显式排除/挂载范围、原始CLI参数与NUL导出；34项相关测试通过，实际私有命名空间 bind/卸载/根重绑定与未知mountinfo失败验证通过。

两票在613d0df整合；格式/全目标检查及23项相应整合测试通过。
