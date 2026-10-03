# 07-independent-validation
Status: ready-for-agent
Category: enhancement
Blocked by: 02,03,04,05,06
Owner: 独立验证

## Scope
黑盒跨进程oracle、损坏/取消/停止与有界资源；Windows 原生与模拟证据分开。

## Acceptance
- [ ] 对应spec门槛通过，并记录确切SHA/环境/命令/实际结果。
- [ ] 符合冻结interface与文件所有权，不覆盖他方/用户配置。
- [ ] 必要公开行为回归先red后green，所有失败/未测项明确。

## Comments

### 2026-10-02 本轮独立证据

04/05嵌入式引擎范围已有新Engine的真实跨进程字面oracle与原生资源/overflow证据；657412e0eec11d762537df4c34193ee24844b18b同SHA两平台CI成功（run37045846370）。用户CLI尚未实现，本任务完整黑盒产品流程验收不标done；不能将119/96全量测试通过视为生产规模/长期/掉电验收。

2026-10-03 范围更新：当前仅支持 Windows；Linux／Unix 实现与验收门槛已取消。以上历史评论属于对应旧提交，不能替代 Windows-only 代码的当前验证。
