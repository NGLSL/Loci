# Windows 验证范围

> 本文描述原有有界目录验证引擎及其历史证据。4096 条目预算不适用于当前 NTFS 全卷服务的产品上限；服务／普通权限 CLI 的当前入口见 [README](../README.md)。
当前项目仅支持 Windows。历史通过数量属于各自提交，不代替本次 Windows-only 代码的精确提交验证。

## 检查入口

```powershell
cargo +1.99.0 fmt --check
cargo +1.99.0 check --all-targets --offline --locked
cargo +1.99.0 test --offline --locked
cargo +1.99.0 test --release --offline --locked
```

`tests/windows_events.rs` 使用工程内真实文件及原生通知；共享 Engine 的可控 EventSource 明确属于模拟 loss／竞态测试。停止、句柄释放、持久化损坏／替换失败与跨进程恢复的历史结果见 [PERSISTENCE.md](PERSISTENCE.md)。CLI 应验证真实 root／database、帮助、错误、查询、分页和监控停止行为。

## 历史证据

基础提交 `7415280` 的 Windows debug／release 各 32 项通过；该轮使用真实文件增删与 rename，通知显式模拟。初始扫描／构建竞态、旧读者版本一致、预取消／执行中取消、重启、模拟漏事件、失败保留旧结果与风暴退避有回归。

基线 CLI 的百万合成记录检查为 12 查询 × complete／first50，共 24 组 count／checksum 与独立 Python oracle 一致；没有逐一比较完整 ID 集合。这不是百万真实文件或百万条实时更新验证。历史有界增量与 Windows 性能证据见 [INCREMENTAL.md](INCREMENTAL.md) 和 [BENCHMARKS.md](BENCHMARKS.md)。

## 结果边界

Validated 仅对应最后观察截止点，文件系统可随后变化。取消或 first50 的部分结果不能称 complete；预算、持续写入及读者保留旧代时可返回明确标记的旧快照。停止或 drop 后留下的 query handle 显示 Stopped。调用方仍须定期 poll，不能把未继续 poll 的快照称为持续校正。

百万真实文件、长期压力、非 NTFS／网络文件系统、真实断电、对抗性 TOCTOU 和生产调用方集成尚未完成验证。Windows NTFS 独立原型的小型原生结果见其固定提交报告，不能等同公共 Engine 已装配。
