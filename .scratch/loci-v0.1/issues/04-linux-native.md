# 04-linux-native
Status: done
Category: enhancement
Blocked by: 01
Owner: 独立Linux适配/验证

## Scope
基于冻结接口与既有Native明确适配；真实loss恢复与资源，精确提交验收。

## Acceptance
- [x] 本任务范围的原生装配/loss恢复与资源门槛通过，并记录确切SHA/环境/命令/实际结果；整体spec其他门槛另行验收。
- [x] 符合冻结interface与文件所有权，不覆盖他方/用户配置。
- [x] 必要公开行为回归先red后green，所有失败/未测项明确。

## Comments

### 2026-10-02 新 Linux Engine 装配与原生验收

功能分支 codex/loci-linux-engine-v0.1；基线 de8f897eda0ba856b1f59e9497f2bd079d287686。
公开 Engine::open 重启回归在099894ccde638fcebaf621d28b3711a0ca54ba69的Linux run37044712939实际红：Unsupported。
代码657412e0eec11d762537df4c34193ee24844b18b的run37045846370两平台均success。
Linux：Ubuntu24.04/x86_64/kernel6.17.0-1022-azure/ext4/Rust1.99.0；debug/release全量各119passed/0failed/6ignored；linux_engine11（10行为+1helper）、共享engine15、storage9、新来源unit2、Engine原生kernel/rootunit2均通过。
新Engine实际观察KernelOverflow（max_queued_events16384），Pending保留旧查询，重建并收敛；仅测试Gate等待时间受控，事件/loss非注入。另验证UserOverflow、递归注册/普通rename增量、依赖rename校正、排除监听退休、非UTF8失败恢复、root身份失败、watch/fd及打开错误释放。
进程计数与实际/procfd、fdinfowatch检查一致；FD+2、三目录3watches，stop/drop/超128watch打开失败回到基线。
两个独立进程间保存与离线文件/目录增删改名后重开通过，实际Unix rename+父目录fsync。
独立规范/规格审计的重建失败误报Stopped、逻辑wd退休预算及rootFIFO竞态风险已修复并补回归，复核无剩余可行动发现。
Windows同SHAdebug/release各96passed/0failed/2ignored，engine16/storage9/windows_events20通过；公开EventSource契约保持poll/stop两个方法。
固定1.99CI命令见.github/workflows/native-engine.yml；公开环境与结果见docs/PERSISTENCE.md。main本轮未改，用户.zcode/.claude ignore保留未提交。
Linux6ignored为4旧性能measure+2旧原型overflow；没有冒充新Engine证据。完整v0.1CLI/产品规模/长期/掉电/其他filesystem和架构未验收。
