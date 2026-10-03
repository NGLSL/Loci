# Loci

Loci 是 Windows 文件名／路径搜索项目，提供独立 **LociIndex** 后台服务、普通权限查询 CLI，以及保留的有界目录验证引擎。当前构建目标仅为 Windows，Rust **1.99.0**；服务 JSON 协议使用 `serde` 和 `serde_json`，依赖说明见 [THIRD_PARTY.md](THIRD_PARTY.md)。许可证尚未决定。

## 后台服务与普通权限查询

`loci-service.exe` 通过 Windows SCM 运行，服务名为 `LociIndex`，账户为 LocalSystem，负责本机固定 NTFS 卷的索引、USN 增量和持久恢复。安装／启用服务需要管理员授权；日常 `loci.exe`、Kite 及官方插件以普通权限通过本机 `\\.\pipe\Loci.Search.v1` 只读查询／状态协议访问同一索引。客户端不能指定目录、保存或重建索引。

## 独立安装包

Loci 独立产出 Kite 插件包 [`loci-kite-plugin.zip`](https://github.com/NGLSL/Loci/releases/latest/download/loci-kite-plugin.zip)。在支持 ZIP 导入和 Loci 文件搜索的 Kite 构建中，直接导入下载的 ZIP，再从 Loci 插件点击安装服务，接受一次管理员授权。旧版仅支持目录导入时，先解压再导入含 `plugin.json` 的目录；旧版不会自动获得新的文件搜索接入。插件包内附 `loci-setup.exe`；服务默认安装目录为 `C:\Program Files\Loci`，注册独立的 Windows 卸载信息。Kite 构建无需克隆 Loci，日常查询无需管理员权限。插件导入不自动提权，也不随 Kite 卸载服务。

开发者只克隆 Loci 即可生成独立 Windows x64 安装包，需要 Rust 1.99.0 和 NSIS：

```powershell
.\scripts\build-installer.ps1
```

输出 `target\package\loci-kite-plugin.zip`、`loci-setup.exe` 和各自的 `.sha256` 文件。插件 ZIP 根目录包含 `plugin.json`、`kite-plugin-loci.exe`、`loci-setup.exe`，以及 SDK 来源、许可证和第三方许可记录。独立 CLI 用户也可直接运行安装器，无需安装 Kite。

安装器注册并启动 `LociIndex` 服务，在 Windows 应用列表中独立显示 Loci；卸载时停止并删除本安装目录所属的服务，保留 `%ProgramData%\Loci` 索引数据，不修改 PATH。首次索引期间查询可能显示准备状态。安装后在普通 PowerShell 中查询：

```powershell
& "$env:ProgramFiles\Loci\loci.exe" status
& "$env:ProgramFiles\Loci\loci.exe" query --type documents --limit 50 report
```

安装与升级固定使用受保护的原生 `Program Files\Loci`，不支持自定义目录；安装脚本核查目录、祖先和已有执行文件的归属及权限，拒绝可被普通用户改写的路径或重解析点。安装与卸载互斥运行。遇到其他目录所属的 `LociIndex`，安装器会拒绝覆盖；若此前安装过 Kite 内置的服务，需要先卸载原归属的服务，再安装独立 Loci。

[`Windows package`](https://github.com/NGLSL/Loci/actions/workflows/release.yml) workflow 可在 GitHub Actions 手动生成构建产物；版本匹配的 `v*` tag 在检查、测试与打包通过后发布插件 ZIP、安装器和 SHA256 文件。添加 workflow 不代表已经发布版本，实际服务安装、升级和卸载仍需 Windows 验收。

## 源码构建与服务管理

构建服务与客户端：

```powershell
cargo +1.99.0 build --release --locked --bin loci-service --bin loci
```

开发构建不能直接从 `target` 注册 LocalSystem 服务。请用独立安装器安装；需要手动修复已安装服务时，在管理员 PowerShell 中运行：

```powershell
& "$env:ProgramFiles\Loci\install-service.ps1" -ExecutablePath "$env:ProgramFiles\Loci\loci-service.exe"
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

Loci 插件作为服务的只读客户端，安装操作单独启动随包附带的安装器。Kite 的构建与安装不依赖 Loci 仓库或服务二进制；文件搜索需要导入 Loci 插件并安装、启动服务。Loci 的插件包、安装、升级和卸载由本项目维护。

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
