# 11: 监控所有者持续运行，查询可取消且能有界关闭

Task: LOCI-LINUX-001-11
Type: task
Parent: LOCI-LINUX-001
Publication: published — 用户已确认粒度和依赖
Status: resolved

**What to build:** 用户保持一个监控会话运行时，可持续查询和取消耗时工作；退出后监控、工作线程、读者和写入资源正确收尾。

**Blocked by:** 10 — 重启先提供旧索引，再校正离线变化

**Acceptance criteria:**

- [x] 后台工作有明确单一监控所有者，定期推进原生事件和校正；持有 query handle 不等于监控所有权。
- [x] 通过 CLI 会话及公开 handles 展示并发查询/状态，长查询与建库不会让监控永久停摆；不强制安装系统服务。
- [x] 运行中查询取消、扫描取消、关闭请求与 writer flush 有界且有明确完成状态；保留不可变结果只标 Stopped。
- [x] 线程、队列、租约、fd/watch/数据库锁资源有预算，慢读者造成背压时无无界内存增长。
- [x] CLI 的输入退出/信号关闭、重复停止和 drop 均可复现验证，不靠制造文件变化才能关闭。

**Testing boundary:** 公开监控/查询生命周期与真实 CLI 会话子进程；原生资源释放和执行中取消。

**User stories covered:** 14, 23, 27, 28, 29, 31

## Comments

- 2026-10-02：依据既有 Linux 规格，按 to-tickets 准备独立本地 ticket 草稿；确认后 Status 改为 ready-for-agent 并逐票发布。父规格和既有 01 任务未修改。


- 2026-10-02：用户批准“按现有拆分发布本地 issues”，随后授权按依赖开发并统一 review。

- 2026-10-02：10已整合ee2e9f3；开始单一后台monitor owner、有界查询/关闭及CLI生命周期。

## Answer

b1fc632 已合入。Engine::spawn单一后台monitor owner，20ms生产cadence，进程最多8owners/每owner8commands；独立QueryHandle，save/rebuild明确等待/超时状态，cancel与stop走独立标志。stop超时保留worker所有权，is_joined/Drop实际join后才表示资源释放。CLI取消非阻塞输出，固定1前台查询任务，无detached stdinreader；EOF/stop/SIGINT/SIGTERM有界关闭，Pending不保存新库并报告unsaved，保留旧checkpoint。45项debug相关测试和9项release生命周期测试通过；合入24项owner/CLI/stale/recovery复验通过，Windows GNU正常/FFI类型检查通过（非原生）。真实慢stdout仍持续更新、锁/fd/watch释放及受控阻塞OS超时边界有测试。证据/workspace/linux-ticket-11-validation.md及docs/LINUX-CLI.md。
