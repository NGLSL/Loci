# 增量分支验证入口与边界

此分支保留原有 rescan 实现作为对照，不改变静态 LOCIEXP2 查询格式。新增 incremental 模块；当前工作树与基础查询共用 Rust 1.99.0 标准库实现，未接入应用调用方。

## 可供独立验证的接口

Portable::new(root, Limits) 建立工程内有界根。Portable::enqueue(Change::Refresh(path))、Remove(path)、Rename {from,to} 接收相对路径，再调用 tick(Duration) 推进显式测试时钟。Windows是真实文件操作+显式通知，不是Windows原生监听。

查询仍为 store.handle().lease().search(query, first50, cancel, progress)，返回不可变版本、准确PathBuf、匹配数与开始/结束状态。Native::new(root, Limits).tick() 在x86_64 Linux解析真实inotify事件；tick_with_hooks允许夹具在库存更新及索引构建后注入竞态。Native::invalidate(Signal) 是模拟loss入口，不能当真实kernel overflow证据；restore(Inventory) 后先做完整校正。

工作量计数：metrics.full_scans、subtree_scans、scanned_entries、metadata_calls、transactions、changed_paths；Store.builds、rebuilt_records、rebuilt_partitions与last_rebuilt_*。全量扫描包括启动校正；扫描计数与索引重新编码的记录数分开。

CLI仅增加一个现有实验的有界入口：live-check explicit-root query duration-ms incremental|rescan。持续时间1..10000ms，仍使用默认4096条目/128目录/16深度/256事件/4重试预算。输出版本、匹配数量和工作量，不输出用户路径。没有做完整CLI产品。

## 当前实现

64个不可变hash分片，每片包含已排序路径及原有Index。新增/删除/rename只重新编码发生变化的分片，其它分片共享Arc。查询合并各片lexicographic前50项；complete总数仍精确，first50匹配数最多50。批次原子发布，取消和最多8租约/两代快照背压继续适用。

库存映射/候选路径及分片比较仍有O(n)的有界复制和检查；这不是O(1)增量成本或百万条在线索引。上限保持4096条目及1MiB输入，没有通过调高上限宣称规模化。

文件事件只定点stat；目录子树rename重映射路径与已有watch，不遍历磁盘子树。新进入目录先装watch再扫描该子树；删除移除子树及watch。可靠事件不调用root全量重扫。启动/重启、kernel/user overflow、未知wd、不可解释顺序、预算或I/O失败等才回到有界root校正，另有30秒保险审计。

cookie对跨poll短暂不完整时先保持Pending，等待至多50ms收集同伴，再按move-out/move-in处理。重复cookie、倒置的已配对事件或孤立self事件不猜测路径，要求校正。完整性基于最后观察切面，不能称无延迟最新状态。

## Windows实际验证

2026-10-02，Windows 11 / NTFS、Intel Core i5-13490F（10核16逻辑处理器）、约32GiB内存，Rust1.99.0，release优化与LTO。debug/release各45项普通测试通过，2项显式测量默认ignored；fmt和all-targets linux-ffi-check通过。后者只证明Rust类型检查，不证明FFI或Linux内核运行。

新增13项便携回归覆盖文件增删rename、目录子树rename、新目录局部扫描、瞬时路径和批次、队列溢出/模拟loss、事务竞态、跨线程旧代读者与两代背压、预算失败/重启、非法路径、查询计数/排序及预取消/运行中取消。没有遍历用户目录。

复现成对测量：

```powershell
python scripts/measure-incremental.py
```

每个规模的每个模式只运行一次，共六个独立子进程；每个进程创建一个目录及对应数量真实小文件，交替rename同一文件31次。每次持有旧查询租约，更新后必须准确命中新路径并处于Validated，再验证ext:rs总数。首列延迟包含rename、显式通知、更新、准确查询；update列只计tick。使用虚拟时间跨越250ms冷却，没有等待真实Windows监听或Linux轮询。顺序在1024规模反转，未做cache flush、CPU绑核或统计置信区间。分位数为31样本排序后的ceil((n-1)*p)。

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

## Linux独立复验（当前未执行）

main基础提交7415280此前53项Linux overlayfs结果不自动算此分支通过。此分支新增6项普通原生用例及2项ignored入口：文件事件、目录watch映射/删除、move-in/out、构建后竞态、队列溢出/背压/重启、现有CLI；目录用例断言/proc实际3watches/1fd保持、删除为1/1、drop为0/0，并对照进程预算计数。另有真实kernel overflow到同一Recovery驱动增量查询，以及原生6组计时。仅类型检查成功，所有这些运行结果均待Linux独立复验。

已具备Rust1.99.0、Bash、timeout与Python3的x86_64 Linux可运行（脚本不安装软件或修改sysctl）：

```sh
bash scripts/validate-linux.sh --incremental-overflow --incremental-metrics
```

普通debug/release应分别包含基础53+便携13+原生6=72项，实际数量与失败以输出为准，不能把预期计数当通过。ignored真实overflow最多10000轮/10秒，没观察到真实IN_Q_OVERFLOW输出SKIP并使脚本报告INCOMPLETE。计时使用真实inotify、5ms轮询并包含250ms冷却，不能直接与上述Windows虚拟时钟延迟比较；性能循环不调用独立全树oracle，普通正确性测试才调用。子进程预算为512MiB地址空间/60CPU秒/120秒墙钟/16MiB输出/256fd，采样RSS/HWM不含内核watch/slab内存。

还未测：ext4/Btrfs/网络文件系统、长期风暴、权限变化的真实Linux运行、对抗性TOCTOU、千万条或百万条在线更新、生产调用方生命周期与停止状态。inotify拆批的孤立MOVE_SELF仍保守校正；异常路径顺序不能承诺始终局部更新。没有持久增量WAL，重启先扫描有界根。
