# Windows NTFS 独立性能原型实测

日期：2026-10-03（Asia/Shanghai）。状态：普通权限性能验证通过；新原型的管理员 USN／恢复验证待授权执行。阶段 B 的既有原生验收不替代本报告的验收。

## 基线与范围

- 分支：`codex/windows-ntfs-performance`。
- 工作树：`D:\Project\Loci\.scratch\worktrees\windows-ntfs-performance`。
- 分叉基线：`0589af74a59480eefeb3d2801e0000bcd44cdc3f`；阶段 B 原生正确性源码：`e9684d5988b1b963c2091426e998b7f35eeb18b8`。
- 本轮实测源码提交：`6df213a54628f65c1d351796cec1271366afa8af`，runner 记录 `source_dirty=False`；release exe SHA-256 `0870CB904255F1E3263761F91C4440E1E4BC2B02F3F3560FA1479B9D36F90CFF`。后续报告提交不改变实测源码。
- main 核对为 `e204917644fb771e5c92e74d130a34127ca8f74e`；用户的 main `.gitignore` 修改和未跟踪任务文件保留。阶段 B 工作树保持干净。
- 新 Cargo 项目 `loci-ntfs-performance`，Rust 1.99.0、edition 2021、std-only、独立 workspace／lock／target。只读复用阶段 A 的 checkpoint、model、win 模块；阶段 A／B 源码、根 Cargo 和共享核心均未改动。
- `.scratch/windows-ntfs-performance/spec.md` 被跟踪，仅本地 `target/`、`run/` 被忽略。没有 main 合并、push、PR、release 或共享 ADR 接受操作。

## 已实现的路径

1. `enumerate.rs` 用文档化的 `GetFileInformationByHandleEx(FileIdBothDirectoryRestartInfo/FileIdBothDirectoryInfo)`，以 64 KiB 对齐批次得到 parent、完整 legacy 64-bit file reference、raw UTF-16 name 和 attributes。逐文件身份打开由目录批次代替；目录本身仍需打开、pin 和前后身份检查，`identity_opens=0` 仅指消除了逐目录项打开，并不代表零系统调用。
2. `query.rs` 一次建立常驻路径视图，保留 UTF-16 排序、大小写敏感 literal 匹配、完整匹配数，只为前 limit 个结果分配返回值。路径 payload 由两张索引共享 Arc；硬链接仍是不同 EntryId 和搜索路径。空查询直接读取索引长度并取前 limit 项。
3. `transaction.rs` 维护 object／parent/name 关系，首次修改时才保存旧目录项；同步失败回滚库存、namespace 和 cursor，查询缓存只在完整验证与 prepare 成功后更新。生产 replay 不再 clone 全库 Snapshot。目录移动会更新受影响子树的缓存路径。
4. 原生 backend 保留 USN 开始边界、重放到稳定截止点、journal／卷／scope 校验、跨进程恢复和原子检查点。原生能力拒绝保留实际 OS code，失败不能成为空库存或 Ready。
5. `mft-projection` 是独立、显式 opt-in 的 FSCTL_ENUM_USN_DATA 诊断：整卷读取仅限 D:，64 KiB 批次、2,000,000 records／20 秒预算，只保留工程 scope parent 的目录项。它不发布完整库存，也不证明全部硬链接名称；范围外记录仅在批次解码时短暂存在，不写入库存、报告或日志。

当前初始化仍是**工程范围批量目录枚举 + USN 重放**。MFT 投影不是 Everything 的直接 MFT 完整建库实现，也没有 raw MFT 全名称解析。`Snapshot::validate` 仍检查完整图；拓扑变化时仍会遍历库存清除移出子树。局部 undo／query update 不能被解释成所有同步操作都达到 O(delta)。

## 环境与权限实测

当前进程观察：Windows 11 教育版 x64，10.0.26200／build 26200；`rustc 1.99.0 (b940084d7 2026-09-28)`。

D: 文件系统 NTFS，legacy serial `0xf228c46d`，volume GUID `\\?\Volume{3b1b89c0-c213-495c-b692-f1e2d9987c9d}\`。普通 token：`process_token_elevated=false`，`process_envbox_module_loaded=false`。这些是本次进程／API 观察，不声称独立验证了整个宿主环境。

- 零访问卷句柄可打开；QUERY、ENUM、READ（无 journal ID 时使用 nonblocking sentinel）分别拒绝，实际 OS code 1。
- GENERIC_READ 打开卷拒绝，实际 OS code 5；整体 capabilities 返回失败，未发布库存。
- 零访问 FSCTL 生命周期 20 次，句柄 97 → 97。capabilities 整个进程首次运行 95 → 96，不能用该一次初始化结果证明泄漏或完全无增长。
- 普通权限的 FILE_ID_BOTH 批量目录枚举实际可用；本轮没有原生 USN journal ID／record version 观察，需管理员 lane。
- 非 NTFS 本轮未实机测试；不支持／拒绝应明确报错。产品降级仍建议目录扫描 + ReadDirectoryChangesW，后者缺少 USN 离线重放／卷级恢复语义，需重新扫描补齐。

## 同夹具普通权限性能结果

证据目录（本地保留、不提交用户文件名）：

`.scratch/windows-ntfs-performance/run/native-20261002T222309Z-efe2bcfb5449461e8e1f730aae08e523/`

提交前预验证的 `native-20261002T221755Z-91236e5813ed4d68a2e5dc6888904e21/` 也保留；下表使用提交后、源码干净且记录了 exe hash 的第二次运行。

runner 使用 release、offline、locked，创建新的真实工程夹具：1,000／10,000 普通数据文件加场景项；实际搜索目录项 1,045／10,125。每次扫描核对完整 EntryId → attributes map，以及独立目录遍历的完整 raw UTF-16 路径集合；不是 count／checksum／前 50 项替代验收。

扫描对照为同一夹具的 `read_dir + 每项 win::identity` 与批量 provider；三次交替扫描后取中位数。查询对照为同一库存每次 `store::paths + filter + 收集全部匹配 + take(50)` 与一次建好的常驻索引。查询 baseline 32 次、optimized 128 次，预热后采样。结果只代表这个机器、这个小型暖缓存夹具，扫描 p95 仅三次样本，不能用于尾延迟承诺。

| 实际目录项 | 扫描 baseline p50 | 批量扫描 p50 | 比率 | 索引一次构建 |
|---:|---:|---:|---:|---:|
| 1,045 | 78.90 ms | 13.17 ms | 5.99× | 2.376 ms |
| 10,125 | 665.63 ms | 128.85 ms | 5.17× | 19.556 ms |

逐项身份打开：1,045／10,125 → 0；目录批次：31／111。目录打开／身份校验成本包含在扫描时间内。

| 10k 查询（limit 50） | baseline p50 | 常驻索引 p50 | 常驻索引 p95 | 比率 |
|---|---:|---:|---:|---:|
| 空查询，全部 10,125 匹配 | 6.263 ms | 1.2 µs | 4.0 µs | 5,219.5× |
| 短词，10,001 匹配 | 6.633 ms | 373.4 µs | 442.8 µs | 17.76× |
| 完整路径片段，1 匹配 | 6.171 ms | 52.0 µs | 139.4 µs | 118.67× |
| literal／Unicode，1 匹配 | 6.402 ms | 284.1 µs | 345.2 µs | 22.53× |
| 不存在的词 | 5.904 ms | 19.8 µs | 83.6 µs | 298.18× |
| 原始 UTF-16 D800，1 匹配 | 6.140 ms | 473.1 µs | 587.9 µs | 12.98× |

空查询收益来自直接读取数量，并不能推广到任意搜索。普通 literal 查询仍线性检查常驻路径，不是百万级倒排／trigram／SIMD 索引。本轮未运行或测量 Everything，不能据此声称相当于或超过 Everything。

## 资源成本

索引构建前后测量均保留 baseline 与 optimized 库存，未测 allocator 净活跃堆；构建临时分配及 allocator 保留空间也可能计入。Working set、private commit 和 peak working set 分列，内核开销／私有工作集未测。

| 目录项 | 路径 UTF-16 payload | 索引构建 WS 增量 | private commit 增量 | 索引句柄增量 | 整个 bench peak WS |
|---:|---:|---:|---:|---:|---:|
| 1,045 | 52,368 B | 1.734 MiB | 1.563 MiB | 0 | 8.934 MiB |
| 10,125 | 503,968 B | 17.160 MiB | 16.555 MiB | 0 | 35.145 MiB |

bench 完整进程句柄均为 89 → 89，包含创建夹具、路径／身份 oracle、两份库存与重复采样；不能拿它的峰值当产品引擎占用。常驻索引保留多张 BTreeMap 和重复 EntryId 名称，当前内存布局不适合直接外推百万规模。下一阶段先测净活跃堆并压缩／intern object、entry、name、parent 数据，而不是只放大上限。

整条 bench 命令分别耗时 786 ms／8,511 ms；采样部分 447 ms／3,925 ms。这些包含多个对照样本和校验，均不是建库时间。真实 Backend 建库、同步、保存、重开恢复耗时等待新原生 lane；旧阶段 B 的 12.632 秒整体验收包含夹具与 oracle，不能作为新原型性能对照。

## 正确性与未验证项

- release 40／40 测试通过，完整场景保留硬链接、Unicode、raw UTF-16、失败回滚、原子文件失败、cursor／provenance 失效、目录移动与 scope 剪枝。审查发现并修复 release 中 `debug_assert!(map.insert(...))` 会删除副作用的问题，增加优化构建的完整查询集合回归。
- 真实目录 batch／逐项身份／全路径 oracle 对照通过，fixture 实际创建 ADS、Unicode 和 D800 原始名称；ADS 不成为额外搜索目录项。
- 真实 rename／add 后的扫描与库存回滚测试通过，但该测试的 replay hook 和 journal 7／cursor 3 是**故障注入**，并非真实 USN 记录。它证明回滚机制，不证明内核同步。
- journal 回卷／重建／游标失效的 codec/provenance 测试为注入；没有修改真实 journal。新原型管理员 USN 建库期间并发 add/delete/file rename/directory rename、跨进程离线变更和所有路径／ID 校验仍待执行。
- 本轮尚未执行卷级 MFT projection。阶段 A 历史 D: 已超过 216 万记录，2M 预算可能拒绝；拒绝须记录明确错误，不能提高预算或标为 EOF 完成。即使投影完成也不覆盖全部硬链接名称。
- 长路径、reparse、大小写敏感目录在新的 native acceptance 中分别有创建／能力标记，尚不能继承阶段 B 的通过结果。新原型真实 V3／128-bit NTFS namespace、非 NTFS、journal 不可读后的真实恢复、长期监听／压力和全部硬链接范围恢复未验证；legacy namespace 以外明确拒绝。
- 没有自动创建 100k／1M 文件、读取个人目录、改变 journal 或系统设置，也没有 GUI／共享引擎装配。

## 复现

在本工作树使用 PowerShell：

```powershell
pwsh -NoProfile -File probes/windows-ntfs-performance/verify.ps1
```

ordinary runner 创建唯一 run／target，执行 fmt、release 40 项测试、build、能力拒绝检查、1k／10k 同夹具性能比较。所有证据保留于它输出的 `evidence=` 路径。

完成授权后，管理员 USN／恢复复验命令（会发起一次 UAC）：

```powershell
pwsh -NoProfile -File probes/windows-ntfs-performance/verify-ntfs.ps1 -RequestElevation
```

只有另行明确授权 D: 有界只读整卷记录枚举，才添加 `-AllowVolumeEnumeration`。projection 的拒绝独立记录，不跳过后续 10k USN 验证，不把诊断失败标为投影完成。默认管理员 lane 仍不枚举整卷 MFT。

## 共享接口建议与下一阶段

沿用 `docs/WINDOWS-SHARED-HANDOFF.md`，ADR 仍 proposed：来源／卷身份与 scope 分离，object 与 EntryId 分离，parent/name 保存原始名称，Windows cursor 保存 journal ID 与 USN，Linux cursor 为平台可选 opaque payload。checkpoint 原子保存与当前数据同步成功是不同状态；查询视图应明确观测版本／Pending，不把失败库存发布为完整。

本轮平台私有 namespace/cache/undo API 不要求共享核心接受同样实现。并发查询下的发布／旧视图生命周期仍需共享 ADR 对齐；当前独立原型靠唯一 `&mut Backend` 保证没有查询者看到事务中间状态。

下一步预算：先完成新原型管理员 1k／10k correctness，再对齐共享接口；优化紧凑存储和查询布局并测净堆后才提 100k 真实条目计划。100k 建议单独授权：工程夹具、总文件／目录／字节预算、创建／删除时间、磁盘可用空间、USN retention 余量、停止后恢复、完整独立路径 oracle 分别计量。1M 须由 100k 的实测成本决定，不线性承诺速度，不用合成字符串冒充真实文件监听。

## 官方 API 依据

- [FILE_ID_BOTH_DIR_INFO](https://learn.microsoft.com/en-us/windows/win32/api/winbase/ns-winbase-file_id_both_dir_info)：目录批次中的 file ID、属性、原始名称和 offset。
- [GetFileInformationByHandleEx](https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-getfileinformationbyhandleex)：目录信息 class 和原生句柄查询。
- [FSCTL_ENUM_USN_DATA](https://learn.microsoft.com/en-us/windows/win32/api/winioctl/ni-winioctl-fsctl_enum_usn_data) 与 [MFT_ENUM_DATA_V0](https://learn.microsoft.com/en-us/windows/desktop/api/winioctl/ns-winioctl-mft_enum_data_v0)：文档化、有界的 MFT 记录枚举路线。

这些文档支持 API 用法，不替代本地 correctness／性能证据。
