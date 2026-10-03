# v0.1 持久化与引擎生命周期

> 本文描述原有有界目录验证引擎及其历史证据。4096 条目预算不适用于当前 NTFS 全卷服务的产品上限；服务／普通权限 CLI 的当前入口见 [README](../README.md)。
提供可嵌入的 `storage::Snapshot` 和 `engine::Engine`，仅装配 Windows 原生递归通知。用户 CLI 使用同一有界 Engine；生产规模及完整 v0.1 交付门槛仍需后续验收。

## 调用流程

原目录引擎使用 Rust 1.99.0 标准库；当前项目服务协议另依赖 serde／serde_json。调用方明确选择一个目录；引擎 canonicalize root，先安装真实来源，再读库、扫描、构建查询快照并再次排空事件。查询结果保留原始拼写，返回 canonical root 与相对路径组合的具体绝对 `PathBuf`。

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

并发边界：调用方须在保存期间保持被选 root 与数据库目录的位置稳定。Windows 原路径保存不提供目录描述符相对写入或全面对抗性 TOCTOU 保证。

`Engine::open` 仅在 Windows 装配原生来源；项目仅支持 Windows 构建。`with_source` 是已有 EventSource 接口的外部装配入口，调用方必须先为同一 canonical root 安装满足契约的来源。源错误保留 `io::Error` 和原 OS code；WatchLost 要求显式重新打开引擎。

## Windows 用户 CLI

CLI 的真实目录入口使用同一有界 Engine，数据库必须位于 ROOT 之外：

```powershell
cargo +1.99.0 run --offline --locked -- engine help
cargo +1.99.0 run --offline --locked -- engine build D:\SelectedRoot D:\LociData\selected-root.loci
cargo +1.99.0 run --offline --locked -- engine query D:\SelectedRoot D:\LociData\selected-root.loci "报告 ext:rs" --fresh
cargo +1.99.0 run --offline --locked -- engine query D:\SelectedRoot D:\LociData\selected-root.loci "ext:rs" --all --page-size 128
cargo +1.99.0 run --offline --locked -- engine watch D:\SelectedRoot D:\LociData\selected-root.loci
```

`watch` 从标准输入读取 `query QUERY`、`export QUERY`、`status`、`rebuild`、`save`、`stop`；stop 或 EOF 保存最后已验证快照并释放监控。默认查询最多保留 50 条路径；`--all`／export 固定一个快照并有界分页。`--null` 输出 NUL 分隔的路径，否则输出转义字符串。状态与错误写 stderr；退出码为 0 成功、2 参数错误、3 失败、4 待校正／不完整。CLI 与有界 Engine 的存在不代表生产规模验收完成。

## 状态、查询与停止

- `Validated` 仅表示最后观察的可靠切面；文件系统可在之后变化。
- `Pending` 表示需要校正，持续事件、loss 或四次尝试耗尽时保留旧版本。
- `Failed` 表示操作失败。运行期间的失败保留旧查询版本；来源已失败时必须重新打开。
- `ReadersPinned` 表示旧代仍由读者持有，按两代预算暂停发布；释放租约后后续 poll 可继续。
- `Stopped` 明确表示监控已停止。`stop` 幂等；drop 同样停止并释放来源与 root 句柄。留下的 query handle 和租约仍能读取不可变版本，开始/结束验证标记为 false。

查询 handle 不拥有监控循环。最多八个查询租约；每个租约固定到取得时的不可变版本。完整查询保留前 50 个具体路径并计算匹配总数；first50 可以提前终止，`complete` 为 false。取消后的 `complete` 也为 false。`complete` 表示遍历完整性，`validated_at_start_and_finish` 表示观察有效性，两者分别判断。

引擎在每次捕获和发布前核对 root 的稳定文件身份。Windows 以持有的目录句柄及 volume/file index 核对路径，不只比较字符串；root 被移走、替换或变为 reparse point 时不会把旧监听句柄对应的结果发布为当前有效。Windows 原生递归通知本身不保证报告 root 自身变化。身份核对依据 [GetFileInformationByHandle](https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-getfileinformationbyhandle)。

loss 批次的 changes 全部丢弃。可靠事件复用原有增量事务；依赖目录 rename、overflow、未配对 rename 与来源重启间隙进入有界全量校正。未在库存中的来源替换已索引目标时，先退休目标旧库存/监听，再安装监听并重新枚举 incoming 子树，不能因为目标仍是 Directory 就复用旧 inode 的状态。扫描及构建后的新事件使候选失效。成功尝试冷却 250 ms，失败指数退避至 2 秒，另有 30 秒低频完整校正。

Windows 暂存 old-name 等待配对时，每次返回均报告 `UnpairedRename`，防止把已消费 old-name 后的空批次当成可靠切面；保留一个 old-name，最多 100 ms，仍允许后续读取中的可靠 new-name 配对。真实 kernel/user/invalid loss 清除暂存名称。持续不完整输入只保持 Pending，不自动反复重建来源。

## 持久格式与安全边界

`Snapshot::new(root, inventory)`、`save(path)`、`load(path, root)` 保存一个完整库存。相对路径使用原始 UTF-8 拼写，包含 File/Directory 类型；查询数据按库存确定重建。当前不支持非 UTF-8 查询候选；拒绝候选并保留已有有效版本，不有损转码。

`LOCISNP1` v1 为单体容器：

| 字段 | 编码 |
| --- | --- |
| magic | 8 bytes，`LOCISNP1` |
| version | little-endian u32，1 |
| platform | little-endian u32，Windows=1；旧非 Windows 平台库不兼容 |
| payload length | little-endian u32 |
| root | u32 UTF-8 长度及 canonical root bytes |
| entry count | little-endian u32 |
| 每条记录 | u8 kind（File=0，Directory=1），u32 路径长度及 UTF-8 相对路径 bytes |
| checksum | little-endian u64，FNV-1a，覆盖此前全部 bytes |

校验用于检测意外损坏，不是认证。历史 `LOCIEXP2` / `LOCWATCH1` 和不兼容版本/平台明确提示重建，无隐式迁移。解析先限制总读取量，再检查长度、校验、数量、kind、UTF-8、路径安全与库存一致性；不按未经检查的 u32 申请内存。

固定上限为 4096 条库存记录、1 MiB 相对路径输入、4096 bytes 单路径与 root、128 目录（包含 root）、16 层目录、256 事件及四次扫描尝试。拒绝绝对/逃逸/NUL/空组件、Windows ADS、重复路径、缺失或非目录父项、排除目录与不完整库存。排除规则复用 scanner/incremental 的规则；不跟随子树 symlink/reparse。

保存先验证并序列化，然后以 `create_new` 语义创建同目录专属临时文件，写入、flush、sync 后原子替换。临时名称碰撞尝试有界，已有或外来临时文件不会被清理。Windows 使用 [MoveFileExW](https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-movefileexw) 的 REPLACE_EXISTING / WRITE_THROUGH，不允许跨卷 copy/delete。替换前失败保留旧有效库。未进行真实断电测试，不宣称跨所有文件系统的断电耐久性。

Windows 来源占两个 OS handles；运行中的引擎另持有一个 root 身份句柄，身份检查临时再打开一个目录句柄。Windows 最多八个原生来源。RSS/进程工作集不包括内核对象字节，不据此声称内核占用已经测量。

## 验证入口与证据范围

```powershell
cargo +1.99.0 fmt --check
cargo +1.99.0 check --all-targets --offline --locked
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

上述表格和 Windows 后续整合段落是历史提交的证据，不代表本次 Windows-only 改动已通过检查。真实断电、长期运行、其他文件系统及生产调用方集成尚未验收；许可证和 release 尚未决定。
