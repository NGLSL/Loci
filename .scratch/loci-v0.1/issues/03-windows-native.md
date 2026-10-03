# 03-windows-native
Status: ready-for-agent
Category: enhancement
Blocked by: 01
Owner: 独立Windows实现

## Scope
只改windows_events及测试；真实事件、rename配对、overflow/错误/stop/drop和进程资源。

## Acceptance
- [ ] 对应spec门槛通过，并记录确切SHA/环境/命令/实际结果。
- [ ] 符合冻结interface与文件所有权，不覆盖他方/用户配置。
- [ ] 必要公开行为回归先red后green，所有失败/未测项明确。

## Comments

### 2026-10-02 持久化与 Windows 生命周期实现

功能分支：codex/loci-persistence-lifecycle-v0.1。
代码验收提交：1828276cc154a7cf808989feac70bb1391a53672。
最终文档提交：f86a280519a2b5cf436698e815e912450b98070f。
Windows 原生来源来自精确提交 13b1f940b8f1d099ff6b06af02981d95796ab21d；整合后修复 old-only 空批次观察窗口。

环境：Rust 1.99.0 / x86_64-pc-windows-msvc / D: NTFS。
命令：cargo +1.99.0 fmt --check；cargo +1.99.0 check --all-targets --offline --locked；cargo +1.99.0 check --offline --locked --features linux-ffi-check；cargo +1.99.0 test --offline --locked；cargo +1.99.0 test --release --offline --locked。
结果：静态检查通过，debug/release 各89 passed、0 failed、2性能测量ignored；storage9、engine16、windows_events13。
规范与规格两轴审计完成；Stopped + WatchLost 状态问题实际 red/green 修复，无剩余可行动发现。

本轮 Windows 对应行为已实现/验证：版本化有界库存与原子保存、损坏和失败保留旧库、真实跨进程重开后离线增删改名校正、原生可靠事件局部更新、root身份失效、租约背压、stop/drop剩余查询Stopped。
整体 v0.1 验收项保持未勾选：Linux新EventSource/原生引擎装配、Unix持久化执行、单个old/new实际跨completion证据、CLI、联合精确SHA跨平台与CI仍待后续。不可将本轮Windows证据当作上述门槛通过。
公共用法与证据：docs/PERSISTENCE.md。用户配置保留在工作树，不纳入提交；main、release、许可证均未变动。

### Windows 分支后续提交整合

补充来源：245b0dfae49a907f599c69101de800d91857c5cc；整合代码：e3d86e28a80581701557ce5b3397df4eefb88f59；最终文档提交：de8f897eda0ba856b1f59e9497f2bd079d287686。
新增7项completion/时钟注入边界测试（非真实跨内核completion证明），Windows原生测试共20项=13真实平台夹具+7注入状态测试。独立两轴增量审计无新可行动发现；debug/release全量96 passed、0 failed、2性能测量ignored；fmt/all-targets/linux-ffi-check通过。两个测试注入入口在lib test harness中有dead_code warning，生产构建不包含此入口。
最后只含Git已提交文件的干净源码目录release构建及engine16/storage9/windows20共45项回归通过。用户ignore配置继续留在本地；本轮未补Linux/CLI/CI门槛。
