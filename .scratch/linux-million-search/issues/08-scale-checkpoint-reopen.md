# 08: 规模库存可保存并在新进程重开

Task: LOCI-LINUX-001-08
Type: task
Parent: LOCI-LINUX-001
Publication: published — 用户已确认粒度和依赖
Status: resolved

**What to build:** 用户能保存十万级库存，并在另一个进程重新查询；写入失败、损坏和不兼容数据不会覆盖可恢复旧库。

**Blocked by:** 07 — 十万真实目录项建库、搜索与局部更新

**Acceptance criteria:**

- [x] 单独版本化的新持久格式保存原始名称、身份/父子关系、来源/范围与一致的发布边界；旧格式仍可读取或明确要求重建。
- [x] 两个独立进程间保存和重开核对完整集合，涵盖原始 bytes、硬链接、链接类型和超过 4096 条库存。
- [x] 更新与检查点/增量提交边界一致；截断、恶意长度和损坏的恢复有界，不能盲信存储游标。
- [x] 临时创建、替换、清理与目录同步保留 Linux 已修复的目录绑定；并发写者锁定/串行化，失败保留有效旧版本。
- [x] 用可控进程终止验证崩溃后重开或明确重建，不将其称为掉电耐久性；仅 CLI 文件命令就能演示。

**Testing boundary:** 公开保存/重开与 CLI 子进程；恶意库、写入失败及受控 crash-restart。

**User stories covered:** 23, 25, 26, 29, 30, 32

## Comments

- 2026-10-02：依据既有 Linux 规格，按 to-tickets 准备独立本地 ticket 草稿；确认后 Status 改为 ready-for-agent 并逐票发布。父规格和既有 01 任务未修改。


- 2026-10-02：用户批准“按现有拆分发布本地 issues”，随后授权按依赖开发并统一 review。

- 2026-10-02：07已合入de42368，开始本票独立开发。

## Answer

75ba93c 已整合09并合入。独立LOCISCL1格式保存原始bytes/父子图/身份与范围；长度、版本、checksum、图循环/父ID等有界校验。目录fd绑定atomic保存复用01保护；持有sidecar flock、旧模式短锁并在锁内校验格式/来源避免降级覆盖。原生与外部source共享恢复/锁生命周期。9项常规checkpoint测试通过；精确提交真实100k/2000目录跨进程保存重开与独立CLI全集合核对通过（2.68秒为整项测试耗时，不是启动性能）。合入28项相关回归通过，十万opt-in在普通复验中忽略但实现阶段已实际执行。SIGKILL证明进程崩溃锁释放，不称掉电耐久性。证据见docs/LINUX-SCALE-CHECKPOINT.md。
