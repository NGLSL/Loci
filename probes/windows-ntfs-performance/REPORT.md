# Windows NTFS 独立性能原型实测

日期：2026-10-03（Asia/Shanghai）。状态：普通权限性能对照与新原型管理员 1k／10k USN／跨进程恢复验证通过。整卷 MFT projection 未执行，仍为显式 opt-in 的独立诊断。阶段 B 的既有原生验收不替代本报告的验收。

## 基线与范围

- 分支：`codex/windows-ntfs-performance`。
- 工作树：`D:\Project\Loci\.scratch\worktrees\windows-ntfs-performance`。
- 分叉基线：`0589af74a59480eefeb3d2801e0000bcd44cdc3f`；阶段 B 原生正确性源码：`e9684d5988b1b963c2091426e998b7f35eeb18b8`。
- 本轮实测源码提交：`6df213a54628f65c1d351796cec1271366afa8af`，runner 记录 `source_dirty=False`；release exe SHA-256 `0870CB904255F1E3263761F91C4440E1E4BC2B02F3F3560FA1479B9D36F90CFF`。后续报告提交不改变实测源码。
- 管理员验收来源：`766ae8810f03c61e653a3837a8727a3f5e916884`（只追加报告，Rust 与上一提交相同），`source_dirty=False`，release exe SHA-256 `61B25960C91CC76444995BF9B986E2DA1898CB5D39D222E33F9578B95D9BA958`。
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
- 普通权限的 FILE_ID_BOTH 批量目录枚举实际可用。管理员 GENERIC_READ 真实 QUERY／READ 可用，journal ID `133340641254344880`，QUERY 支持范围显示 major 2..4；本轮实际消费记录只有 V2。parser 支持 V2/V3 不等于验证过真实 V3/V4；V4／未知记录明确拒绝。
- 管理员 ENUM 的 EOF sentinel 返回 OS 38，仅证明该能力调用被接受，不能当作真实整卷枚举验收。QUERY 起始 first USN `37731958784`、next USN `37768626960`；实际原生验收使用各自开始边界和保存游标。
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

整条 bench 命令分别耗时 786 ms／8,511 ms；采样部分 447 ms／3,925 ms。这些包含多个对照样本和校验，均不是建库时间。旧阶段 B 的 12.632 秒整体验收包含夹具与 oracle，不能作为新原型性能对照。

## 新原型管理员原生验收

用户继续指令后启动一次 UAC，明确仅执行工程夹具 USN lane；没有传入 `-AllowVolumeEnumeration`。父进程观察 `elevated_child_exit=0`，所有 fmt／release 40 项测试／build／capabilities／两组 build 与 recover 命令 exit 均为 0，runner `NativeComplete=true`。

证据：`.scratch/windows-ntfs-performance/run/native-20261003T053027Z-efb3ca1829d04a8aabba1a1ec61d95c5/`。管理员进程 token elevated=true，EnvBox module loaded=false。journal 身份与普通 lane 的卷 serial／GUID 一致；没有 journal 或系统配置变更。

| 实测层次 | 1k 数据文件 | 10k 数据文件 |
|---|---:|---:|
| Backend 建库调用（包含初始 save_new） | 135 ms | 382 ms |
| bootstrap 内部统计 | 131 ms | 372 ms |
| leaf add/delete/rename 同步 | 2 ms | 5 ms |
| directory rename + hardlinks 同步 | 3 ms | 9 ms |
| scope 移入／移出同步 | 4 ms | 9 ms |
| 重开后的离线 USN replay | 3 ms | 9 ms |
| recover 验收区间：重开、oracle、保存、重读 | 172 ms | 1,364 ms |
| 整条 acceptance-build 命令 | 1,270 ms | 10,556 ms |
| 整条 acceptance-recover 命令 | 177 ms | 1,377 ms |
| 初始建库并发变更轮数 | 25 | 70 |

`save_new` 已包含在 Backend build 调用；后续保存也位于 recover 验收区间，本轮**没有单独隔离保存耗时**，不得从上表倒推出纯保存／纯重开耗时。热查询延迟来自上一节普通 lane 的独立采样。建库与先前 B 日志并非同一时刻、同一变更调度的成对测量，不承诺固定加速倍率。

两组每个阶段均用独立目录遍历的完整路径集合校验：并发 bootstrap、leaf rename/add/delete、directory rename/hardlink、scope move、reparse、跨 parent 硬链接增加／删除、工程外侧测试来源的首次 scope 内 alias、alias 删除、范围外工程噪声、跨进程离线恢复。噪声同步均 scoped_records=0、directories_visited=0。reparse 只保留链接目录项，不跟随目标。

1k 初次扫描遇到真实 OS 2，候选库存未发布，重试一次后成功；10k 无该重试。取消使用真实 cancel flag，旧库存／cursor／query 保持不变、Pending 禁止保存；stopped query 行为也通过。

增量资源证据：两组 leaf 同步仅 1 directory batch、4 undo entries、4 cached paths 更新；directory rename 分别 6 undo entries、108／208 个旧新路径更新；跨进程离线恢复仅 2 directory batches、12 undo entries、14 cached paths 更新，full_scans=0。这些证明局部工作量，并不取消完整图验证成本。

独立 Python checker 在管理员进程退出后，以普通权限重新遍历两组静止夹具并逐项核对：

- 全部 raw UTF-16 搜索路径集合及目录项属性一致；最终分别 1,046／10,126 项，真实数据文件分别 1,000／10,000。
- 所有 legacy volume/object identity、scope、root identity、volume GUID、checkpoint checksum 一致；`checkpoint_simulated=false`。
- 离线旧名称精确消失、新名称存在；源对象与新硬链接精确两条路径，真实 link count=2；scope 移出／移入目录变化符合预期。
- D800 原始名称保留，最长绝对路径分别 406／407 UTF-16 units，各有 4 条路径超过 260 units。ADS 已实际创建，但不成为额外目录项；新原型大小写敏感目录未测试。
- 1k 保存 cursor `37768936792`，10k 保存 cursor `37771706640`；inventory 文件分别 71,713／688,515 bytes。两份 JSON `independent-check-1000.json`／`independent-check-10000.json` 保留所有布尔结果，不保存名称清单。

资源：四个 acceptance 进程均 handles 56 → 56。bootstrap 活跃 WS／private commit：1k 8.813／4.133 MiB，10k 35.398／31.328 MiB；完整 build 命令 peak WS 9.410／38.594 MiB，recover peak WS 9.082／37.906 MiB。活跃资源包含 Backend 索引与夹具并发线程／进程运行成本，峰值另含 oracle／保存／重读；内核开销与 private working set 未测。

## 正确性与未验证项

- release 40／40 测试通过，完整场景保留硬链接、Unicode、raw UTF-16、失败回滚、原子文件失败、cursor／provenance 失效、目录移动与 scope 剪枝。审查发现并修复 release 中 `debug_assert!(map.insert(...))` 会删除副作用的问题，增加优化构建的完整查询集合回归。
- 真实目录 batch／逐项身份／全路径 oracle 对照通过，fixture 实际创建 ADS、Unicode 和 D800 原始名称；ADS 不成为额外搜索目录项。
- 真实 rename／add 后的扫描与库存回滚测试通过，但该测试的 replay hook 和 journal 7／cursor 3 是**故障注入**，并非真实 USN 记录。它证明回滚机制，不证明内核同步。
- journal 回卷／重建／游标失效的 codec/provenance 测试为注入；没有修改真实 journal。新原型管理员 USN 建库期间并发 add/delete/file rename/directory rename、跨进程离线变更、完整路径和最终所有 ID／属性校验已真实通过。
- 本轮尚未执行卷级 MFT projection。阶段 A 历史 D: 已超过 216 万记录，2M 预算可能拒绝；拒绝须记录明确错误，不能提高预算或标为 EOF 完成。即使投影完成也不覆盖全部硬链接名称。
- 长路径、reparse、Unicode、原始 UTF-16、ADS 已在新夹具实测；大小写敏感目录未验证。新原型真实 V3／128-bit NTFS namespace、非 NTFS、journal 不可读后的真实恢复、长期监听／压力和任意硬链接范围的完整性未验证；legacy namespace 以外明确拒绝。本夹具多路径硬链接验证不能推广为任意整卷硬链接建库。
- 没有自动创建 100k／1M 文件、读取个人目录、改变 journal 或系统设置，也没有 GUI／共享引擎装配。

## 复现

在本工作树使用 PowerShell：

```powershell
pwsh -NoProfile -File probes/windows-ntfs-performance/verify.ps1
```

ordinary runner 创建唯一 run／target，执行 fmt、release 40 项测试、build、能力拒绝检查、1k／10k 同夹具性能比较。所有证据保留于它输出的 `evidence=` 路径。

本次使用的管理员 USN／恢复复验命令（手动执行会发起一次 UAC）：

```powershell
pwsh -NoProfile -File probes/windows-ntfs-performance/verify-ntfs.ps1 -RequestElevation
```

独立 checker 命令，fixture／checkpoint 值取对应证据目录 `result-1000.txt` 或 `result-10000.txt`：

```powershell
python probes/windows-ntfs-performance/independent-check.py <fixture> <checkpoint>/inventory.lcusn --count 10000 --verify-native-fixture
```

只有另行明确授权 D: 有界只读整卷记录枚举，才添加 `-AllowVolumeEnumeration`。projection 的拒绝独立记录，不跳过后续 10k USN 验证，不把诊断失败标为投影完成。默认管理员 lane 仍不枚举整卷 MFT。

## 共享接口建议与下一阶段

沿用 `docs/WINDOWS-SHARED-HANDOFF.md`，ADR 仍 proposed：来源／卷身份与 scope 分离，object 与 EntryId 分离，parent/name 保存原始名称，Windows cursor 保存 journal ID 与 USN，Linux cursor 为平台可选 opaque payload。checkpoint 原子保存与当前数据同步成功是不同状态；查询视图应明确观测版本／Pending，不把失败库存发布为完整。

本轮平台私有 namespace/cache/undo API 不要求共享核心接受同样实现。并发查询下的发布／旧视图生命周期仍需共享 ADR 对齐；当前独立原型靠唯一 `&mut Backend` 保证没有查询者看到事务中间状态。

下一步预算：新原型管理员 1k／10k correctness 已通过，优先对齐共享接口；优化紧凑存储和查询布局并测净堆后才提 100k 真实条目计划。100k 建议单独授权：工程夹具、总文件／目录／字节预算、创建／删除时间、磁盘可用空间、USN retention 余量、停止后恢复、完整独立路径 oracle 分别计量。1M 须由 100k 的实测成本决定，不线性承诺速度，不用合成字符串冒充真实文件监听。

## 官方 API 依据

- [FILE_ID_BOTH_DIR_INFO](https://learn.microsoft.com/en-us/windows/win32/api/winbase/ns-winbase-file_id_both_dir_info)：目录批次中的 file ID、属性、原始名称和 offset。
- [GetFileInformationByHandleEx](https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-getfileinformationbyhandleex)：目录信息 class 和原生句柄查询。
- [FSCTL_ENUM_USN_DATA](https://learn.microsoft.com/en-us/windows/win32/api/winioctl/ni-winioctl-fsctl_enum_usn_data) 与 [MFT_ENUM_DATA_V0](https://learn.microsoft.com/en-us/windows/desktop/api/winioctl/ns-winioctl-mft_enum_data_v0)：文档化、有界的 MFT 记录枚举路线。

这些文档支持 API 用法，不替代本地 correctness／性能证据。
