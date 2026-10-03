# Windows 有界增量验证入口与边界

> 本文描述原有有界目录验证引擎及其历史证据。4096 条目预算不适用于当前 NTFS 全卷服务的产品上限；服务／普通权限 CLI 的当前入口见 [README](../README.md)。
本文保留固定历史提交的 Windows 结果；项目现仅支持 Windows，Linux Native 实现与验收入口已移除。

此分支保留原有 rescan 实现作为对照，不改变静态 LOCIEXP2 查询格式。新增 incremental 模块；当前工作树与基础查询共用 Rust 1.99.0 标准库实现，未接入应用调用方。

## 可供独立验证的接口

Portable::new(root, Limits) 建立工程内有界根。Portable::enqueue(Change::Refresh(path))、Remove(path)、Rename {from,to} 接收相对路径，再调用 tick(Duration) 推进显式测试时钟。Windows是真实文件操作+显式通知，不是Windows原生监听。

查询仍为 store.handle().lease().search(query, first50, cancel, progress)，返回不可变版本、准确PathBuf、匹配数与开始/结束状态。restore(Inventory) 后先做完整校正。Linux Native 入口已移除；真实 Windows 原生来源使用 Engine。

工作量计数：metrics.full_scans、subtree_scans、scanned_entries、metadata_calls、transactions、changed_paths；Store.builds、rebuilt_records、rebuilt_partitions与last_rebuilt_*。全量扫描包括启动校正；扫描计数与索引重新编码的记录数分开。

实际目录监控使用当前 `engine watch ROOT DATABASE`，数据库须位于 ROOT 之外；标准输入可发送 `query QUERY`、`export QUERY`、`status`、`rebuild`、`save` 和 `stop`。例如：

```powershell
cargo +1.99.0 run --offline --locked -- engine watch D:\SelectedRoot D:\LociData\selected-root.loci
```

该入口使用 Windows 原生来源及默认 4096 条目／128 目录／16 深度／256 事件／4 次扫描重试预算。原型增量／重扫计时仍由 `python scripts/measure-incremental.py` 复验；脚本构建并运行 `tests/incremental.rs` 中默认 ignored 的 `measure_incremental_comparison`，按 256／1024／3072 真实文件执行成对测量。该计时使用显式通知与虚拟冷却时钟，不能作为 `engine watch` 原生通知延迟证据。

## 当前实现

64个不可变hash分片，每片包含已排序路径及原有Index。新增/删除/rename只重新编码发生变化的分片，其它分片共享Arc。查询合并各片lexicographic前50项；complete总数仍精确，first50匹配数最多50。批次原子发布，取消和最多8租约/两代快照背压继续适用。

库存映射/候选路径及分片比较仍有O(n)的有界复制和检查；这不是O(1)增量成本或百万条在线索引。上限保持4096条目及1MiB输入，没有通过调高上限宣称规模化。

Portable 显式事件事务复用分片快路径；可靠事件局部更新，依赖 rename、预算或 I/O 失败进入有界 root 校正。真实 Windows 通知的来源行为与 Portable 模拟通知分别验证。

同一批次中，若较早事件的路径与较后rename源路径重合、互为祖先或后代，且最终目标仍是目录，先标GenerationRace并保留旧查询为Pending，再执行有界root校正。原因是事件携带的是事件发生时路径，而定点stat看到的是批次结束后的磁盘状态；直接套用会遗漏先创建后随父目录迁移的文件，或误用复用路径的替代子树。此保守规则也可能让部分本可增量处理的依赖批次回退；独立事件、单次rename，以及目标为普通文件或已消失的短暂文件rename批次仍使用原有分片快路径。

## 初始39790ae的Windows测量与自带验证

2026-10-02，Windows 11 / NTFS、Intel Core i5-13490F（10核16逻辑处理器）、约32GiB内存，Rust1.99.0，release优化与LTO。debug/release各45项普通测试通过，2项显式测量默认ignored；fmt 与 all-targets 检查通过。

新增13项便携回归覆盖文件增删rename、目录子树rename、新目录局部扫描、瞬时路径和批次、队列溢出/模拟loss、事务竞态、跨线程旧代读者与两代背压、预算失败/重启、非法路径、查询计数/排序及预取消/运行中取消。没有遍历用户目录。

复现成对测量：

```powershell
python scripts/measure-incremental.py
```

每个规模的每个模式只运行一次，共六个独立子进程；每个进程创建一个目录及对应数量真实小文件，交替rename同一文件31次。每次持有旧查询租约，更新后必须准确命中新路径并处于Validated，再验证ext:rs总数。首列延迟包含rename、显式通知、更新、准确查询；update列只计tick。使用虚拟时间跨越250ms冷却，没有等待真实 Windows 监听。顺序在1024规模反转，未做cache flush、CPU绑核或统计置信区间。分位数为31样本排序后的ceil((n-1)*p)。

| 文件数 | 模式 | 变更到查询p50/p95 (ms) | update p50/p95 (ms) | 全过程CPU (s) | 峰值工作集 (MiB) | 全根扫描 | 枚举条目 | 重新编码记录 |
|---:|---|---:|---:|---:|---:|---:|---:|---:|

| 256 | rescan | 19.169/19.948 | 18.606/19.395 | 0.719 | 9.371 | 32 | 8224 | 8224 |
| 256 | incremental | 1.358/1.586 | 0.754/0.922 | 0.156 | 9.910 | 1 | 257 | 536 |
| 1024 | rescan | 74.145/80.572 | 73.601/80.056 | 2.781 | 10.285 | 32 | 32800 | 32800 |
| 1024 | incremental | 3.208/3.534 | 2.640/2.956 | 0.578 | 9.785 | 1 | 1025 | 1862 |
| 3072 | rescan | 224.144/245.108 | 223.523/244.553 | 8.391 | 11.059 | 32 | 98336 | 98336 |
| 3072 | incremental | 9.476/12.620 | 8.712/11.613 | 1.859 | 11.180 | 1 | 3073 | 6142 |

扫描与重新编码计数包含启动：增量启动后31次rename的全根扫描数为0，定点metadata调用62次；分别额外重新编码279/837/3069条分片记录。rescan每次重编码全部记录。目录本身也占一个索引记录，所以枚举规模为文件数+1。incremental总126个重建分片包含启动64片及31次两片，rescan的32计数表示32次单体重建，两者不能当相同单位直接比较。

CPU为整个测试子进程的用户+内核时间，包括创建夹具、初次索引、查询断言和清理，不是仅更新CPU；Windows GetProcessTimes的时间粒度有限。5ms采样工作集/PrivateUsage，PeakWorkingSet读取OS累计峰值；分别记录峰值commit约2.523/2.980/4.465MiB（增量）及2.633/3.469/4.891MiB（重扫）。这些不是引擎持久常驻内存，也不包含内核watch对象。原始六组日志和JSON写入本地忽略的results-incremental目录。

3072文件的本次增量p95从245.108ms降至12.620ms，CPU从8.391s降至1.859s；收益主要来自避免31次文件系统全根枚举，不能外推生产吞吐、百万条实时更新或普通查询100ms目标。当前分片复制O(n)和每次扫描64片的查询开销仍需后续规模验证。

## 独立验收与正确性修复

独立Windows harness对39790ae（本次表格的增量版本）debug/release均19 passed / 2 failed / 1 ignored。两项问题不能被性能收益掩盖：同批create-child后parent rename漏记录却发布Validated；Windows Portable显式Refresh将NTFS alternate data stream作为新filename发布。自带45项测试通过并不等于独立正确性验收通过。

Windows路径校验现在在任何事件路径的filesystem I/O之前拒绝ASCII冒号，涵盖Refresh/Remove及Rename源、目标；实际NTFS夹具创建a.rs:stream、验证metadata可访问但read_dir只列a.rs，确认拒绝后旧查询标Failed且通过有界校正恢复。数据流与filename的语义依据[微软File Streams文档](https://learn.microsoft.com/en-us/windows/win32/fileio/file-streams)。祖先rename批次保护已合入，并与Windows ADS规则共同验证；保守规则不猜测依赖子树的内容。

独立性能结果来自相同harness的旧7415280与候选39790ae、每档31次rename、Windows真实文件+显式通知。3072文件的end-to-end p95为230.9216→9.0909ms，整个测试进程CPU为8.03125→1.765625s；peak working set为11.152344→11.207031MiB，略增0.054687MiB。单独精确路径query的p95为0.0073→0.0437ms，约慢5.99倍，绝对值很小且计时噪声影响比例；扫描64分片确实有额外查询成本，不能称所有查询都加速。该harness CPU包括夹具、断言与独立oracle，不能和上表当完全同一口径；旧版扫描32次是无retry均匀夹具的推导计数，候选1次为instrumented计数。

这些Windows独立性能是修复前提交的基准，不作为共同修复版性能验收证据。原始独立报告与JSON留在本地验证目录，不发布本机路径或原始数据。

## 联合P1/P2修复验收

联合工作树在Windows debug/release各51项普通测试通过，2项默认ignored。新P1便携回归先运行得到4项稳定失败，包括published=true/Validated但漏new.rs；合入保护后4项全部通过。P2真实NTFS ADS回归在debug/release通过，fmt 与 all-targets 检查通过。

当前 Windows-only 范围继续保留依赖 rename 与 NTFS ADS 的整批预检：拒绝发生在任何路径 inspection 之前，不让提前校正绕过 ADS 错误。历史测试与性能表不代表本次清理后的精确代码已经复验。
