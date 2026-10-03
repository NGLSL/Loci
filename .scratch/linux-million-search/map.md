# Linux 百万级搜索实施地图

## Notes

用户批准02–18本地任务并授权全部开发后统一review；状态以各issue文件为准。集成分支`codex/linux-million-search`，父规格保留发布时快照。

## Decisions-so-far

- [前置任务：Linux 原生目录替换与数据库保存边界修复](issues/01-native-engine-regressions.md)：已完成；证据见任务文件。
- [02: 用现有引擎跑通真实目录 CLI 闭环](issues/02-bounded-cli-workflow.md)：已在 cfdbe50 实现真实 Engine CLI build/watch/query/status/rebuild；原生 CLI 4/4 通过，Linux/Windows GNU all-targets 类型检查通过。诊断 stderr 与可逆输出分离，当前4096条限制明确。
- [03: 在固定快照上翻页与导出全部匹配](issues/03-snapshot-pagination.md)：7e32f77 实现固定快照公开分页与CLI全量导出；原生137项全集合/旧快照发布竞态、121项CLI导出验证通过。相关34项通过、1项既有忽略；Linux/Windows GNU all-targets检查通过。
- [04: 用目录项身份完成搜索与目录改名闭环](issues/04-entry-identity-rename.md)：60d9db0 引入显式规模模式、父/名称/目录项ID分段库存和快照查询后端；原生硬链接独立删除、嵌套改名/旧租约/后续变化、身份校正通过。27项相关测试通过，Linux及Windows GNU(含FFI)检查通过。该票小型模型验收，不冒充百万/Windows原生验收。
- [05: 搜索原始名称并遵守链接、排除与挂载范围](issues/05-raw-names-links-scope.md)：f90febc 已实现原始名称/合法文本匹配/链接类型/显式排除/挂载范围、原始CLI参数与NUL导出；34项相关测试通过，实际私有命名空间 bind/卸载/根重绑定与未知mountinfo失败验证通过。
- [06: 在两千目录中持续观察并报告覆盖缺口](issues/06-recursive-watch-coverage.md)：e28722e 已实现共享实际监听预算、反向路径查找、明确覆盖缺口/资源报告、有限重试；4项新原生监听测试通过（2000目录、move-in、普通用户权限恢复、停止释放），旧模式限制保留。实际内核ENOSPC未强制触发，错误分类已实现；不将其称为原生ENOSPC验收。
- [07: 十万真实目录项建库、搜索与局部更新](issues/07-real-100k-live-search.md)：de42368 完成真实100k/2000目录全路径oracle、原生增删改名/目录退休、局部分段工作和显式物理预算。release开发基线：建库0.297s、完整分页0.160s、2001watches、孤立变化1.8–4ms；含oracle测试进程RSS45/HWM62MiB，不冒充生产百万RSS门槛。24项相关测试、Linux/Windows GNU检查通过。每owner物理预算已实现；共享进程内存分配器留给13。详见docs/LINUX-SCALE-100K.md。
- [09: 漏事件与覆盖变化后有界校正并保留旧查询](issues/09-bounded-loss-recovery.md)：f3097a4 已合入。实际内核 IN_Q_OVERFLOW（queue16384）、普通 uid1000 权限撤销/恢复、模拟持续 loss 有限重试、取消/恢复、旋转目录覆盖审计和旧结果保留均已验证。实现 RecoveryOptions 资源/重试预算、独立累计 observed_losses，以及 CLI cancel/rebuild 语义。实现阶段50项相关测试通过；合入26项恢复/CLI/Engine/scope/watch复验通过，Linux/Windows…

- [08检查点](issues/08-scale-checkpoint-reopen.md)：75ba93c独立格式、写者锁、目录绑定保存及100k跨进程全集合验证通过。

- 用户确认统一review基点e204917，覆盖原生修复和全部规模改动。

- [10旧快照启动](issues/10-stale-first-startup.md)：ee2e9f3 首次扫描前Pending可查询，CLI fresh/分离计时及离线/启动竞态验证通过。

- [11后台monitor owner](issues/11-monitor-owner-lifecycle.md)：b1fc632 生命周期、慢输出持续监控与真实释放/超时语义通过，12/13开始。

- [12查询](issues/12-responsive-first-page.md)：f3986dd生产/测量代码，b4c77f2文档；39×200真实100k每类最差p95 4.779512ms，全分页/精确count/全排序oracle正确；原始前后日志保留，非百万/RSS/nativeWindows门槛。

- [13合并和空闲](issues/13-bounded-compaction-idle.md)：57cca5b生产/测量、8d43763文档；真实1M完整600.09s CPU0.282%/core、RSS168.39MiB/HWM183.83MiB，20,001watches及实际退出释放。100k生产200/class事件p95约22.4ms，无额外fullroot；先前失败partial日志保留。

## Fog

- Btrfs隔离TCGguest挂载能力已证明，最终SHA行为/百万验收仍待执行；同提交Windows原生运行尚缺；本地Windows项目可见，但当前会话无创建/派发本机会话工具。
- ext4私有loop挂载已可运行，vda标为ROTA=1，不能声称已满足SSD/NVMe参考硬件。
- 真实24小时墙钟门槛要在最终生产SHA完成，短跑/脚本不算通过。
- 当前02–13完成，14正式实施中；规模和原生最终门槛与统一review仍未完成。
