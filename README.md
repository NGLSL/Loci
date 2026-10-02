# Loci

Loci 是独立的 Rust 文件名/路径搜索实验，包含紧凑路径记录、块级 trigram 候选过滤、短词摘要及最终精确匹配。实时原型将 Linux inotify 接到不可变查询快照，支持有界增量更新、失效校正、取消和并发读者；保留全量重扫作为对照。

项目使用 Rust **1.99.0** 和标准库，无第三方 Cargo 依赖，无 GUI。v0.1 嵌入式引擎在 Windows 使用原生递归通知，在 x86_64 Linux 使用 inotify，统一提供版本化持久库存和停止生命周期。用户 CLI 尚未完成。许可证尚未决定，没有添加 LICENSE 文件。

嵌入式 `engine::Engine::open/poll/query/save/stop` 和 `storage::Snapshot::new/save/load` 用法、数据库位置、格式、预算及验证范围见 [docs/PERSISTENCE.md](docs/PERSISTENCE.md)。完整 v0.1 仍须完成用户 CLI、生产规模和产品交付验收。

新引擎代码提交 `657412e` 已在同一次 [Rust 1.99 跨平台 CI](https://github.com/NGLSL/Loci/actions/runs/37045846370) 验证：Linux/ext4 debug、release 各119 passed，Windows/NTFS 各96 passed，均0 failed。Linux 覆盖实际新 Engine 的跨进程保存与离线变化恢复、递归监听、真实内核溢出校正和 watch/fd 释放；此记录与旧原型验收分开。

Linux scale 开发提交 `57cca5b` 在真实百万非根条目上通过完整字节路径 oracle：生产 20 ms owner 的实际进程静默 600.09 秒，CPU 为单逻辑核的 0.282%，RSS 168.39 MiB，HWM 183.83 MiB，20,001 个实际 watches；停止后 watches 为零。另在真实 100k 条目上各测量 200 次新增、删除、重命名，p95 均不超过 22.6 ms，压缩前后全路径一致且没有额外整根扫描。测量 SHA、进程、容量预算与 RSS 的区别及原始记录见 [压缩与静默验证](docs/LINUX-SCALE-COMPACTION-VALIDATION.md)。此处使用原生 inotify/overlayfs；最终 SSD/ext4/Btrfs、百万变更与长跑验收仍待完成。

查询为大小写不敏感的 AND 子串，可使用 `ext:rs` 精确扩展名条件。first50 返回记录顺序前50项，complete 计算完整匹配数并保留前50项；没有相关性排序、全文、拼音或 mmap。

实时原型最多4096项、总UTF-8输入1 MiB；单条路径4096 bytes、查询512 bytes。非UTF-8候选明确失败，旧结果保留并标待校正。最多8查询租约、两代索引；Loci 管理的会话共享128 watches /8 sessions预算。成功尝试冷却250ms，失败退避至多2秒。独立CLI的100k/1M合成记录基准不受实时4096项限制。

```sh
cargo +1.99.0 fmt --check
cargo +1.99.0 test --offline
cargo +1.99.0 test --release --offline
```

已有 Rust 1.99.0、rustfmt、Bash、timeout、Python3 的 x86_64 Linux：

```sh
bash scripts/validate-linux.sh --kernel-overflow --live-metrics
```

Windows可运行共享查询、恢复、持久化与引擎夹具测试；原型通知显式模拟，新引擎及 `windows_events` 使用原生通知：

```powershell
cargo +1.99.0 test --release --offline --target-dir target/stage3
python scripts/measure-live-windows.py
```

Windows的 linux-ffi-check 只能用于 cargo check；不要在Windows用该feature运行测试或构建来冒充Linux执行。

基础提交7415280验收：Windows debug/release各32项；x86_64 Linux overlayfs各53项及另行真实overflow/原生测量通过。联合P1/P2修复Windows各51项通过；云端P1单独补丁Linux各79项通过，精确联合提交仍待复验。成对测量、复验入口与实现边界见 [docs/INCREMENTAL.md](docs/INCREMENTAL.md)。生产规模、其它文件系统和架构尚未验证。

验证方法与当前通过范围见 [docs/VALIDATION.md](docs/VALIDATION.md)；合成数据、硬件与性能局限见 [docs/BENCHMARKS.md](docs/BENCHMARKS.md)；实现归属见 [THIRD_PARTY.md](THIRD_PARTY.md)。
