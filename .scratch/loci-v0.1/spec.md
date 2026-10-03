# Loci v0.1 可用闭环
Status: ready-for-agent
Category: enhancement
Blocked by: None

## Problem Statement
现有原型查询/增量工作量已验证，但没有可日常使用的真实root→可靠持久索引→具体路径结果→停止/重启闭环；Windows原生事件采集缺失，历史格式为trusted实验输入，监控停止状态和用户CLI尚未完成。

## Solution
提供Kite可嵌入的独立Rust1.99文件名/路径module，并用必要CLI验证同一interface。明确支持范围内可建立、持久化、查询具体路径、原生监控、停止并离线重启校正。分阶段完成所有门槛后才称v0.1可用；不合main/发release/选许可证。

## User Stories
1. 调用方明确选择一个root，不扫描全电脑。
2. 调用方索引root中的文件和目录，排除规则统一且可解释。
3. 调用方获得具体原始路径，包含中文/空格，查询归一化不改原名。
4. 调用方用AND子串及ext过滤，了解first50和complete含义。
5. 调用方保存快照并跨进程再次打开。
6. 调用方遇不兼容旧格式时获得明确重建提示，不误读或静默迁移。
7. 调用方遇损坏/截断/超预算库时得到受控错误，不panic/巨量分配。
8. 调用方保存失败时仍能读取旧有效快照。
9. 调用方离线add/delete/rename后，重启先校正再宣称当前结果。
10. Windows调用方不再伪造通知，真实采集事件。
11. Linux调用方在声明平台有真实inotify恢复证据。
12. 调用方遇overflow/不可解释rename能观察Pending并静止后收敛。
13. 查询者持有不可变版本，不混入新代；超租约/两代背压可解释。
14. 查询者能取消，部分结果不当complete。
15. 引擎停止或drop后剩余query handle显示停止监控。
16. 调用方看到预算超限/权限错误，而不是不完整Validated。
17. 新用户用CLI指定真实root/database/query，不需要基准脚本。
18. CLI无参数/非法参数有帮助与受控退出码，无unwrap panic。
19. 验证者能区分合成记录、真实文件、模拟与原生事件的证据。
20. 验证者用确切SHA和平台记录复验，而非继承别的工作树通过结论。

## Implementation Decisions
- 当前实时上限4096条（目录计入）/1MiB输入/128目录/16深度/256事件/4扫描重试，先做明确受限root的可用闭环；不靠调大常量宣称百万实时。更大root需要另行架构/规模验收。
- 使用PathBuf原始路径，root-relative键；Windows ADS不当filename，Linux冒号合法；UTF-8查询候选不能接受时明确失败。
- 模块交接使用EventSource/EventBatch/Change/Loss/SourceState/EventLimits契约；单ownerpoll、Send不要求Sync；error/loss/Stopped不可吞为可靠空批。
- 持久化选择版本化单体快照容器：root绑定、原始相对path+kind、查询数据或可确定重建数据、格式版本、长度边界与完整性校验。先实现可安全重建的格式，不要求WAL。历史LOCIEXP2/LOCWATCH1明确不兼容或有受控导入，不自动假装迁移成功。
- 保存临时文件flush/sync、同目录原子替换及平台验证；损坏候选不能覆盖最后有效库。真实断电保障只按实际测量范围描述。
- 主实现拥有storage/engine/装配、查询生命周期、公共路径/排除策略；Windows方仅拥有原生来源module和测试；Linux方单独适配/验证，避免同时编辑装配。
- library interface隐藏generation/cookie/watch拓扑，CLI调用同一interface；原型模块可暂时兼容保留但不是长期用户流程。
- Rust1.99/std-only优先。是否增加Windows bindings/校验依赖先评估必要性和兼容性，不安装全局软件。

## Testing Decisions
测试seam沿用已授权Index save/load、Engine open/poll/query/save/stop及CLI实际子进程；对原生EventSource使用真实平台fixture，oracle不调用待测scanner/matcher。一次一个用户纵切片，先red再green；损坏库在有界独立进程验证；外部系统时间/通知可注入但产品逻辑不mock。最后规范/规格两轴review和精确SHA跨平台复验。

## Completion Gates
- G1正确性：联合P1/P2精确SHA的独立Windows/Linuxoracle通过；依赖rename不能Validated遗漏。
- G2持久化：真实root保存/重开/具体路径查询/离线变化校正；版本、损坏、截断、预算、写入失败受控，保留旧有效库。
- G3平台：Windows真实native增删/目录rename/停止释放；Linux真实inotify/overflow及watch/fd释放，声明filesystem分别验收。
- G4生命周期：空root/失败/pending/取消/旧reader/Stopped状态准确，drop不留下假监控状态。
- G5资源：声明profile内结果完整，超限明确；普通可靠事件局部更新，校正计数透明。分开实际文件和100k/1M合成记录基准，报告CPU/内存/冷热p50/p95/未测项。
- G6交付：CLI帮助/错误/无结果/输出转义与实际用户流程一致；干净目录构建binary及版本/格式文档；Windows/LinuxCI按固定Rust1.99运行已有依赖。发布release和许可证仍待用户决策。

## Out of Scope
Kite正式集成、GUI、全文、拼音、相关性排序、全盘默认扫描、百万实时已达成、替用户选许可证、自动合main/发release。

## Further Notes
产品化从已交付修复277271a开始；事件契约单独提交后可分派Windows采集。独立验收矩阵只作输入，不伪称现有产品规格已完成。用户AGENTS/skills/配置不盲目公开；过程任务本地.scratch。
