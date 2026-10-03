# P1/P2增量正确性修复
Status: done
Category: bug
Blocked by: None

## Problem Statement
可靠同批子路径事件后祖先rename会漏文件却标Validated；Windows显式ADS输入不属于目录entry却被索引。

## Solution
P1发现事件时间路径与最终磁盘不一致时，保留旧版Pending并有界校正；普通独立rename保留增量快路径。P2只在Windows拒绝带ASCII冒号事件路径，拒绝发生在该路径filesystem I/O前，不影响Linux合法冒号filename。

## User Stories
1. 调用方收到Validated时能按最后观察切面信任完整路径集合。
2. 调用方遇到依赖rename批次能观察Pending，并在静止后恢复正确查询。
3. 调用方在Windows不能将NTFS数据流视为普通filename。
4. Linux调用方仍能查询含冒号filename的增删rename。
5. 独立验收方能按精确Git SHA复验，不继承不同工作树的通过结论。

## Implementation Decisions
复用Portable enqueue/tick、Native tick、QueryHandle lease/search，已有用户授权的测试seam不变。共用依赖检查置于topology变化前；Windows-path规则使用cfg(windows)。不增加依赖，不扩4096上限，不合main，不公开原始日志/技能/配置，不选择许可证。

## Testing Decisions
独立真实文件oracle和已运行最小红灯；P1四项Windows回归先red后green；P2真实NTFS stream可stat但read_dir不列它，安全拒绝后旧结果标失败并校正。Windows全套debug/release、fmt、linux-ffi-check；联合SHA待独立Windows和Linux复验。性能按平台、版本、冷却及CPU作用域区分。

## Out of Scope
产品化持久化、Windows原生watcher、Kite正式集成、GUI全文、许可证/合并/发布release。

## Further Notes
云端P1工作树Linux79pass是单独补丁证据，不能当联合80项已通过。用户gitignore设置必须保留，公开只增加回归文件白名单。

## Implementation Issues

- [01：P1 依赖目录改名批次校正](issues/01-dependent-directory-renames.md) — done。
- [02：P2 Windows ADS 事件路径拒绝](issues/02-windows-stream-event-paths.md) — done。

## Current Verification

2026-10-03 核对确认原始修复已由 `277271a37581ee388cc88b3c0086b067928c43c5` 实现并进入 main；先前 `ready-for-agent` 是任务记录未回写，不是代码未实施。本次没有重复修改共享引擎代码。

Windows 合并树 `94740f7576888ddccae01cbb6ef98c744ce7c04a`：P1 定向四项回归通过；主项目 debug/release 各96 passed、0 failed、2 ignored，覆盖P2真实ADS、ADS与父rename组合、普通独立rename快路径、Engine整批校验。fmt及all-targets linux-ffi-check通过。ignored为原有两项显式性能测量，不是正确性测试。三个独立NTFS原型release分别24/24/40 passed。

复验命令：`cargo +1.99.0 test --offline --locked -- --test-threads=1`，release加`--release`；`cargo +1.99.0 fmt --check`；`cargo +1.99.0 check --all-targets --features linux-ffi-check --offline --locked`。本轮独立target在验证工作树内，不改变4096条目／128目录上限，不安装依赖，不操作真实journal。

本次未重新执行Linux原生回归，Windows上的Linux cfg测试为0项，linux-ffi-check仅证明类型检查。Linux合法冒号名称与原生rename回归保留；历史Linux证据与本次Windows结果分列，不把旧79项或预期80项当作本次联合提交的原生通过。
