# v0.1 持久化与引擎生命周期

本轮提供可嵌入的 `storage::Snapshot` 和 `engine::Engine`，整合 Windows 原生通知。仍不是完整 v0.1 交付：Linux 的新 EventSource 装配、用户 CLI、联合精确提交的跨平台验收和 CI 交付另行完成。原型 CLI 与 Linux Native 模块保留。

## 调用流程

Rust 1.99.0、标准库，无新增 Cargo 依赖。调用方明确选择一个目录；引擎 canonicalize root，先安装递归 Windows 来源，再读库、扫描、构建查询快照并再次排空事件。查询结果保留原始拼写，返回 canonical root 与相对路径组合的具体绝对 `PathBuf`。

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

`Engine::open` 当前使用 Windows 原生来源；其他平台返回 `Unsupported`，不模拟原生监听。`with_source` 是已有 EventSource 接口的外部装配入口，调用方必须先为同一 canonical root 安装满足契约的来源。源错误保留 `io::Error` 和原 OS code；WatchLost 要求显式重新打开引擎。

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

## 验证入口与证据范围

```powershell
cargo +1.99.0 fmt --check
cargo +1.99.0 check --all-targets --offline --locked
cargo +1.99.0 check --offline --locked --features linux-ffi-check
cargo +1.99.0 test --offline --locked
cargo +1.99.0 test --release --offline --locked
```

Windows 原生测试使用明确选择的临时目录与实际文件操作。`tests/windows_events.rs` 包括原生增删、文件/目录 rename、Unicode、非法 UTF-16 原名、实际 kernel overflow、停止/取消完成竞态及句柄计数；长 rename 的多次读取测试不能当作已确定观察到单个 old/new 跨 completion 的证明。`tests/engine.rs` 的外部可控 EventSource 是明确模拟的错误/loss/竞态证据，和原生夹具分别记录。`tests/storage.rs` 的恶意库在有墙钟期限的独立子进程验证。

本轮当前 Linux 新引擎原生装配、Unix rename/fsync 执行、真实断电、长期运行、其他文件系统及生产调用方集成尚未验收。原 Linux 原型历史结果不能替代本轮精确提交的运行验证。许可证、release 和 main 合并仍待用户另行决定。
