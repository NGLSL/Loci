# v0.1 持久化与引擎生命周期

提供可嵌入的 `storage::Snapshot` 和 `engine::Engine`，装配 Windows 原生递归通知与 x86_64 Linux inotify。原型 CLI 与 Linux Native 模块保留；新引擎的用户 CLI、生产规模及完整 v0.1 交付门槛仍需后续验收。

## 调用流程

Rust 1.99.0、标准库，无新增 Cargo 依赖。调用方明确选择一个目录；引擎 canonicalize root，先安装真实来源，再读库、扫描、构建查询快照并再次排空事件。Linux 在枚举每个目录之前安装 watch，新增子树同样遵守这个顺序。查询结果保留原始拼写，返回 canonical root 与相对路径组合的具体绝对 `PathBuf`。

```rust
use loci_experiment::engine::Engine;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize};

let mut engine = Engine::open(
    Path::new(r"D:\SelectedRoot"),
    Some(Path::new(r"D:\LociData\selected-root.loci")),
)?;
let handle = engine.query();
let result = handle.lease()?.search(
    "报告 ext:rs", false, &AtomicBool::new(false), &AtomicUsize::new(0),
)?;
// 调用方定期调用 poll；只有 true 表示最后观察切面已发布。
engine.poll()?;
engine.save()?;
engine.stop()?;
# Ok::<(), std::io::Error>(())
```

数据库父目录必须已存在，数据库及保存临时文件必须在被监控 root 之外，避免把自身写入纳入库存。引擎不创建用户目录、不默认扫描其他 root、不后台隐式保存。`None` 表示内存会话，调用 `save` 会明确报错。缺失库可建立新索引；已有损坏或不兼容库会返回错误，保留原文件，由调用方决定重建。启动扫描错误也返回错误，持久库不会被覆盖。

`Engine::open` 在 Windows 和 x86_64 Linux 装配原生来源；其他平台返回 `Unsupported`，Linux FFI 当前不支持其他架构。`with_source` 是已有 EventSource 接口的外部装配入口，调用方必须先为同一 canonical root 安装满足契约的来源。源错误保留 `io::Error` 和原 OS code；WatchLost 要求显式重新打开引擎。

## 状态、查询与停止

- `Validated` 仅表示最后观察的可靠切面；文件系统可在之后变化。
- `Pending` 表示需要校正，持续事件、loss 或四次尝试耗尽时保留旧版本。
- `Failed` 表示操作失败。运行期间的失败保留旧查询版本；来源已失败时必须重新打开。
- `ReadersPinned` 表示旧代仍由读者持有，按两代预算暂停发布；释放租约后后续 poll 可继续。
- `Stopped` 明确表示监控已停止。`stop` 幂等；drop 同样停止并释放来源与 root 句柄。留下的 query handle 和租约仍能读取不可变版本，开始/结束验证标记为 false。

查询 handle 不拥有监控循环。最多八个查询租约；每个租约固定到取得时的不可变版本。完整查询保留前 50 个具体路径并计算匹配总数；first50 可以提前终止，`complete` 为 false。取消后的 `complete` 也为 false。`complete` 表示遍历完整性，`validated_at_start_and_finish` 表示观察有效性，两者分别判断。

引擎在每次捕获和发布前核对 root 的稳定文件身份。Windows 以持有的目录句柄及 volume/file index 核对路径，不只比较字符串；root 被移走、替换或变为 reparse point 时不会把旧监听句柄对应的结果发布为当前有效。Windows 原生递归通知本身不保证报告 root 自身变化。身份核对依据 [GetFileInformationByHandle](https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-getfileinformationbyhandle)。

loss 批次的 changes 全部丢弃。可靠事件复用原有增量事务；依赖目录 rename、overflow、未配对 rename 与来源重启间隙进入有界全量校正。扫描及构建后的新事件使候选失效。成功尝试冷却 250 ms，失败指数退避至 2 秒，另有 30 秒低频完整校正。

Windows 暂存 old-name 等待配对时，每次返回均报告 `UnpairedRename`，防止把已消费 old-name 后的空批次当成可靠切面；保留一个 old-name，最多 100 ms，仍允许后续读取中的可靠 new-name 配对。真实 kernel/user/invalid loss 清除暂存名称。持续不完整输入只保持 Pending，不自动反复重建来源。

Linux 使用 cookie 配对 rename；未配对 FROM 暂存整个有界批次，最多 100 ms，等待期间每次报告 loss，未配对 TO 不猜测刷新。逻辑 wd 路径随可靠批次推进，物理 watch 随引擎事务更新，支持延后发布；普通可靠目录 rename 保留监听。子目录的不可解释 self-event 或未知 wd 触发校正；root 监听丢失或 unmount 为 Failed。全量校正释放旧 inotify fd、建立新来源、重新递归注册及扫描；重建间隙使原观察失效，直到扫描及最终事件排空成功。

## 持久格式与安全边界

`Snapshot::new(root, inventory)`、`save(path)`、`load(path, root)` 保存一个完整库存。相对路径使用原始 UTF-8 拼写，包含 File/Directory 类型；查询数据按库存确定重建。当前不支持非 UTF-8 查询候选；拒绝候选并保留已有有效版本，不有损转码。

`LOCISNP1` v1 为单体容器：

| 字段 | 编码 |
| --- | --- |
| magic | 8 bytes，`LOCISNP1` |
| version | little-endian u32，1 |
| platform | little-endian u32，Windows=1 / Unix=2 / other=3 |
| payload length | little-endian u32 |
| root | u32 UTF-8 长度及 canonical root bytes |
| entry count | little-endian u32 |
| 每条记录 | u8 kind（File=0，Directory=1），u32 路径长度及 UTF-8 相对路径 bytes |
| checksum | little-endian u64，FNV-1a，覆盖此前全部 bytes |

校验用于检测意外损坏，不是认证。历史 `LOCIEXP2` / `LOCWATCH1` 和不兼容版本/平台明确提示重建，无隐式迁移。解析先限制总读取量，再检查长度、校验、数量、kind、UTF-8、路径安全与库存一致性；不按未经检查的 u32 申请内存。

固定上限为 4096 条库存记录、1 MiB 相对路径输入、4096 bytes 单路径与 root、128 目录（包含 root）、16 层目录、256 事件及四次扫描尝试。拒绝绝对/逃逸/NUL/空组件、Windows ADS、重复路径、缺失或非目录父项、排除目录与不完整库存。排除规则复用 scanner/incremental 的规则；不跟随子树 symlink/reparse。

保存先验证并序列化，然后用 `create_new` 创建同目录专属临时文件，写入、flush、sync 后原子替换。临时名称碰撞尝试有界，已有或外来临时文件不会被清理。Windows 使用 [MoveFileExW](https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-movefileexw) 的 REPLACE_EXISTING / WRITE_THROUGH，不允许跨卷 copy/delete；Unix 使用 rename 和父目录 fsync。替换前失败保留旧有效库。Unix 在替换成功后目录 fsync 失败，返回错误但新库可能已经可见。未进行真实断电测试，不宣称跨所有文件系统的断电耐久性。

Windows 来源占两个 OS handles；运行中的引擎另持有一个 root 身份句柄，身份检查临时再打开一个目录句柄。Windows 最多八个原生来源。RSS/进程工作集不包括内核对象字节，不据此声称内核占用已经测量。

Linux 引擎持有一个 inotify fd 和一个 root 身份 fd，以 Unix dev/inode 核对选定 root。所有 Loci 管理的 inotify 来源共享 8 sessions / 128 watches 上限，包含旧原型会话；超限返回错误。stop/drop 释放来源和 root fd，查询 handle 不持有这些 fd。

## 验证入口与证据范围

```powershell
cargo +1.99.0 fmt --check
cargo +1.99.0 check --all-targets --offline --locked
cargo +1.99.0 check --offline --locked --features linux-ffi-check
cargo +1.99.0 test --offline --locked
cargo +1.99.0 test --release --offline --locked
```

Windows 原生测试使用明确选择的临时目录与实际文件操作。`tests/windows_events.rs` 包括原生增删、文件/目录 rename、Unicode、非法 UTF-16 原名、实际 kernel overflow、停止/取消完成竞态及句柄计数；长 rename 的多次读取测试不能当作已确定观察到单个 old/new 跨 completion 的证明。`tests/engine.rs` 的外部可控 EventSource 是明确模拟的错误/loss/竞态证据，和原生夹具分别记录。`tests/storage.rs` 的恶意库在有墙钟期限的独立子进程验证。

2026-10-02，本轮代码提交 `1828276cc154a7cf808989feac70bb1391a53672` 在 x86_64 Windows/MSVC、D: NTFS、Rust 1.99.0（`b940084d7`，LLVM 23.1.1）验证：

| 检查 | 实际结果 |
| --- | --- |
| fmt / all-targets check / linux-ffi-check | 全部通过；FFI check 仅是类型检查 |
| debug 全量测试 | 89 passed，0 failed，2 ignored |
| release 全量测试 | 89 passed，0 failed，2 ignored |
| 原生来源 | 13 tests，包括真实 kernel overflow；来源持有时 OS handles +2，128 次取消/完成竞态后回到基线 |
| 引擎 | 16 tests，包含两个独立进程间保存/重开、离线 add/delete/rename 及普通可靠变化不增加 full_scans |
| 持久化 | 9 tests，实际 Windows 锁文件使替换失败后旧库仍可读；恶意输入与临时名称碰撞在有 10 秒上限的子进程中拒绝 |
| 两轴审计 | 规范审计发现 Stopped + WatchLost 被误认正常停止，补回归并修复；规格审计无其他可行动发现 |

两个 ignored 测试是现有显式性能测量，不是本轮性能结论。新引擎取消回归使用预置取消；执行中取消的既有覆盖来自共享查询原型。本轮未新增吞吐或内存改善声明。

同步核对期间，Windows 功能分支新增 `245b0dfae49a907f599c69101de800d91857c5cc`；随后整合为 `e3d86e28a80581701557ce5b3397df4eefb88f59`，Windows 两个文件与来源提交一致。新增七项明确注入 completion/时钟的状态测试，覆盖跨 completion old/new、100 ms TTL、损坏记录/尾随 bytes、零字节、插入其他事件及 pending rename 停止。两轴增量审计未发现新问题。该精确代码提交的 debug/release 全量测试均 **96 passed、0 failed、2 ignored**；原生来源测试为 20 项（13 项真实平台夹具及 7 项注入状态测试），引擎仍 16 项、持久化仍 9 项，fmt/all-targets/linux-ffi-check 通过。lib test harness 对仅在独立 integration module 使用的两个注入方法报告 dead_code warning，生产构建无此测试入口。

上述表格和 Windows 后续整合段落是历史提交的证据；原 Linux 原型历史结果同样不能替代新 Linux Engine 的运行验证。新 Linux 验收记录见下文。真实断电、长期运行、其他文件系统及生产调用方集成尚未验收；许可证和 release 尚未决定。

## Linux 新引擎验收

功能分支 `codex/loci-linux-engine-v0.1`；固定 Rust 1.99.0 的 GitHub Actions 工作流 `.github/workflows/native-engine.yml` 在 Linux 与 Windows 对同一提交运行静态检查及 debug/release 全量测试。Linux 的新引擎夹具是 `tests/linux_engine.rs`，共享 `tests/engine.rs` 同时在两平台执行；持久容器测试为 `tests/storage.rs`。

独立红阶段提交 `099894ccde638fcebaf621d28b3711a0ca54ba69` 的 [Linux run](https://github.com/NGLSL/Loci/actions/runs/37044712939) 已实际编译并执行公开 `Engine::open` 重启回归，明确在 `Unsupported` 处失败。装配后的代码验收提交为 `657412e0eec11d762537df4c34193ee24844b18b`，2026-10-02 的 [同提交跨平台 run](https://github.com/NGLSL/Loci/actions/runs/37045846370) 已完成，Linux 和 Windows jobs 均为 success。

新增原生测试覆盖递归注册后子项变更、目录 rename 后保留监听、目录属性事件、依赖 rename 校正、root 身份失败、非 UTF-8 失败恢复、用户事件队列溢出、排除目录反复退休与重新注册、实际 `/proc/self/fd` 和 fdinfo watch 释放，以及两个新进程间保存/离线变更/重开。真实内核溢出测试位于 `engine::linux_kernel_tests`，要求同一新 Engine 消费实际 `IN_Q_OVERFLOW`、保留 Pending 旧查询，再重建并收敛；仅在测试中延后 Gate 校正时间以保留待排空的 fd，没有注入文件事件或 loss。另有两项来源内部回归覆盖实际重建 ENOENT 与逻辑 wd 退休；FIFO/symlink helper 测试验证 root 打开方式，不声称复现了完整 TOCTOU 竞态。

| 验证 | Linux | Windows |
| --- | --- | --- |
| 环境 | Ubuntu 24.04、x86_64 GNU/Linux、kernel 6.17.0-1022-azure、工作区 ext4 | Windows Server 2022、x86_64 MSVC、D: NTFS |
| Rust | 1.99.0（b940084d7，LLVM 23.1.1） | 同版本 |
| fmt / all-targets check | 通过 | 通过，另通过 linux-ffi-check 类型检查 |
| debug 全量 | 119 passed、0 failed、6 ignored | 96 passed、0 failed、2 ignored |
| release 全量 | 119 passed、0 failed、6 ignored | 96 passed、0 failed、2 ignored |
| 新引擎专用夹具 | linux_engine 11 项：10 项行为回归和 1 个子进程辅助入口，全部通过 | 同文件按平台排除 |
| 共享引擎 / 持久容器 | engine 15 项 / storage 9 项，全部通过 | engine 16 项 / storage 9 项，全部通过 |
| 新内核溢出恢复 | debug/release 均实际观察 IN_Q_OVERFLOW，max_queued_events=16384；Pending 旧查询、重建后正确库存 | 本轮沿用 Windows 原生来源与恢复回归 |
| watch / fd 释放 | 实际 fd +2、递归 3 watches，stop/drop/部分打开失败回到基线 | 既有原生来源回归 20 项通过 |
| 两轴审计 | 规范与规格审计发现的重建失败误报 Stopped、逻辑 wd 退休和 root 打开竞态已修复，复核无剩余可行动问题 | 共享装配及路径解释回归通过 |

Linux 的 6 项 ignored 为 4 项既有显式性能测量和 2 项旧原型内核溢出测试；本轮新增的新 Engine 内核溢出测试不是 ignored，已真实执行。Windows 的 2 项 ignored 为既有显式性能测量。没有将任何 ignored 项计入通过数量，没有新增性能改善结论。

Linux 运行实际执行了快照写入、sync、同目录 rename 和父目录 fsync，以及两进程之间的恢复。runner ext4 挂载包含 `nobarrier` 和 `data=writeback`；上述证据仅证明该环境的程序行为，不证明掉电耐久性、其他文件系统、其他 Linux 架构或长期运行。整体 v0.1 的 CLI、生产规模与完整交付验收仍未完成。main 保持上一轮已获授权合并的提交；本轮 Linux 工作同步到功能分支。
