# 实现与参考资料

当前项目直接依赖 `serde`（含 derive）和 `serde_json`，用于本地服务协议及 CLI JSON。Cargo.lock 固定其精确版本与传递依赖；新服务不再是纯标准库项目。源码没有纳入 Cardinal、plocate、FSearch 的实现，也没有 vendor 目录。查询结构、摘要、恢复流程、FFI适配与测试在本实验中编写；Windows FFI 调用系统 API，不包含操作系统源码。

设计研究参考以下官方项目；参考算法与架构没有引入其源代码或作出许可证兼容承诺：

- [Cardinal](https://github.com/cardisoft/cardinal)：Rust/macOS 文件搜索工具，研究其 namepool、查询分段、取消与 mmap 组件。
- [plocate](https://plocate.sesse.net/)：磁盘 trigram/posting-list 架构参考，短查询可能退化扫描。
- [FSearch](https://github.com/cboxdoerfer/fsearch)：Linux 文件搜索工具，参考其文件系统监听与恢复问题。

原型没有复制上述项目代码。任何将来实际复用组件都需要单独记录来源、版本和许可证。

Loci的许可证选择尚未完成。本次没有添加LICENSE文件，也没有把其它项目的许可证套用到Loci。

## 当前 Cargo 依赖

直接依赖为 `serde` 1.0.229、`serde_json` 1.0.151；传递依赖包括 serde_core、serde_derive、itoa、memchr、proc-macro2、quote、syn、unicode-ident、zmij，具体版本以 Cargo.lock 为准。独立安装包将 Cargo 锁定依赖源包中的 LICENSE／COPYING／NOTICE 文件放入安装目录 `licenses/<crate>-<version>/`，并附带本说明。以下算法参考记录不替代实际依赖的许可记录。

## Kite 插件 SDK

`plugin/src/sdk.rs` 复制了 Kite Rust 插件 SDK 的 JSON-RPC 帧与响应助手，来源及固定提交见 [SDK-SOURCE.md](plugin/SDK-SOURCE.md)。原版权和 Apache-2.0 许可证保留在 [SDK-LICENSE](plugin/SDK-LICENSE) 和源码文件头；SDK 不依赖 Kite 仓库的本地路径。插件 ZIP 附带这两个文件、本说明及锁定 Cargo 依赖的许可文件。

## 参考项目的已核实许可来源

- Cardinal：所查revision的MIT文本：[LICENSE](https://github.com/cardisoft/cardinal/blob/4c50734f9a09d88110f96652b43634b412f79449/LICENSE)。
- plocate：官方1.1.25发布包的README将plocate及其对updatedb的改动声明为GPL2或更新版本；继承的updatedb声明GPL2。应以具体文件声明为准：[官方源码包](https://plocate.sesse.net/download/plocate-1.1.25.tar.gz)。
- FSearch：所查主程序声明GPL2或更新版本：[src/fsearch.c](https://github.com/cboxdoerfer/fsearch/blob/d531eb3b50560fb7d9ba731787100d827f4e1e8a/src/fsearch.c)，[LICENSE](https://github.com/cboxdoerfer/fsearch/blob/d531eb3b50560fb7d9ba731787100d827f4e1e8a/LICENSE)。

以上只是参考来源记录，未复制这些项目的源码或把它们的许可证赋予Loci。
