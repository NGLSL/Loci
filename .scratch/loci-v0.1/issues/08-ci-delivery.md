# 08-ci-delivery
Status: ready-for-agent
Category: enhancement
Blocked by: 06
Owner: 可独立文档/CI任务

## Scope
固定Rust1.99 Windows CI与构建打包说明；不发布release、不添加许可证。

## Acceptance
- [ ] 对应spec门槛通过，并记录确切SHA/环境/命令/实际结果。
- [ ] 符合冻结interface与文件所有权，不覆盖他方/用户配置。
- [ ] 必要公开行为回归先red后green，所有失败/未测项明确。

## Comments

### 2026-10-02 必要原生验收CI

新增.github/workflows/native-engine.yml，功能分支push/手动触发，最小contents:read，固定Rust1.99.0与checkout精确SHA；Ubuntu24.04和Windows2022均运行fmt/all-targets与debug/release完整套件，Linux外层timeout、serial harness、实际kernel事件测试和环境/FS记录。657412e0eec11d762537df4c34193ee24844b18b run37045846370成功。产品CLI/交付说明未完成，本任务保持未完成；未发布release或选择许可证。

2026-10-03 范围更新：当前仅支持 Windows；Linux／Unix 实现与验收门槛已取消。以上历史评论属于对应旧提交，不能替代 Windows-only 代码的当前验证。
