# Loci

Loci 是 Windows 文件名／路径搜索项目，提供独立 **LociIndex** 后台服务、普通权限查询 CLI，以及保留的有界目录验证引擎。当前构建目标仅为 Windows，Rust **1.99.0**；服务 JSON 协议使用 `serde` 和 `serde_json`，依赖说明见 [THIRD_PARTY.md](THIRD_PARTY.md)。许可证尚未决定。

## 后台服务与普通权限查询

`loci-service.exe` 通过 Windows SCM 运行，服务名为 `LociIndex`，账户为 LocalSystem，负责本机固定 NTFS 卷的索引、USN 增量和持久恢复。安装／启用服务需要管理员授权；日常 `loci.exe`、Kite 及官方插件以普通权限通过本机 `\\.\pipe\Loci.Search.v1` 只读查询／状态协议访问同一索引。客户端不能指定目录、保存或重建索引。

构建服务与客户端：

```powershell
cargo +1.99.0 build --release --locked --bin loci-service --bin loci
```

在管理员 PowerShell 安装／启用服务：

```powershell
.\scripts\install-service.ps1 -ExecutablePath D:\Project\Loci\target\release\loci-service.exe
```

正常查询无需管理员权限，结果为 JSON：

```powershell
.\target\release\loci.exe status
.\target\release\loci.exe query --type documents --limit 50 report
cargo +1.99.0 run --locked --bin loci -- query --type images --limit 20 holiday
```

`--type` 支持 all、images、documents、videos、audio、archives、folders；`--limit` 为 1..100。独立 CLI 整个请求期限为 6 秒；Kite 插件使用 500 ms 请求期限，两者分别约束。

## NTFS 索引范围和验证边界

后端先通过 MFT 全卷枚举取得对象，再以 `FILE_ID_BOTH_DIR_INFO` 目录批次补齐名称和硬链接关系，USN 变化更新库存，checkpoint 同时保存来源身份、库存与游标。NTFS 内部元数据和精确 `%ProgramData%\Loci` 存储子树排除，防止保存自身触发变化循环；不会排除整个 ProgramData。文件系统或读取错误、持续校正和预算失败显式报告，旧结果不能称为新的完整库存。

未配对 UTF-16 名称在索引中保留原始身份；当前 JSON 输出跳过无法安全表示的名称并报告数量，避免有损转换后打开错误路径。百万合成库已有实跑证据，但真实服务安装、真实全卷／百万文件、普通权限端到端查询、升级和长期运行仍待验收；不能宣称已达到 Everything 性能。

Kite 构建将服务、CLI 与安装脚本打包到 `resources/loci/`，官方插件作为服务的只读客户端。Kite 通过固定 Loci 提交构建服务和插件；更新依赖时应先提交 Loci 实现，再更新 Kite 的固定 revision。远端构建还需要该提交已推送，并验证对应 CI。

## 原有目录验证引擎

`engine::Engine::open/poll/query/save/stop` 和 `storage::Snapshot` 保留用于有界目录验证，文档见 [PERSISTENCE.md](docs/PERSISTENCE.md)。`cargo run` 默认仍运行 `loci-experiment`，原 `engine` CLI 可显式选 root／database、查询、分页和监控：

```powershell
cargo +1.99.0 run --locked -- engine help
```

该旧验证引擎最多 4096 条目、128 目录、16 深度及 1 MiB UTF-8 相对路径输入，最多 8 查询租约和两代索引；这些是验证模块预算，**不是全盘服务的产品上限**。旧目录 CLI 的 root／save／rebuild 不能用于配置当前服务或 Kite 插件。

## 检查与历史证据

```powershell
cargo +1.99.0 fmt --check
cargo +1.99.0 check --all-targets --locked
cargo +1.99.0 test --locked
cargo +1.99.0 test --release --locked
```

[Windows NTFS 原型交接](docs/WINDOWS-SHARED-HANDOFF.md)与[身份／游标 ADR](docs/adr/0001-source-object-entry-cursor.md)保留固定历史实现和 proposed 契约，不因当前服务实现而自动 accepted。历史提交 `657412e` 的 [Rust 1.99 CI](https://github.com/NGLSL/Loci/actions/runs/37045846370) 中 Windows debug／release 各 96 passed；这不能代表当前服务实现已经 CI 或原生安装验收。

旧有界验证边界见 [VALIDATION.md](docs/VALIDATION.md)、历史增量测量见 [INCREMENTAL.md](docs/INCREMENTAL.md)、基准方法见 [BENCHMARKS.md](docs/BENCHMARKS.md)。旧测量不能代替全卷服务性能验证。
