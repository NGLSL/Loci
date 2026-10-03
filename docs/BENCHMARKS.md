# Windows 基准方法与边界

本文保留历史 Windows 测量，不能作为当前 Windows-only 改动的性能验收。

数据为确定性生成的文件记录或测试自身创建的小文件；不使用用户文件名或文件内容。百万记录基准无需创建百万真实文件。mmap、相关性TopK和百万条实时更新不是本轮能力。

## Windows有界桥接计时

2026-10-02，Rust1.99.0 release，i5-13490F（10核/16逻辑CPU）、约32GiB RAM、Windows11、NTFS。未固定CPU频率/亲和性、未清OS缓存、未确认具体物理盘映射，因此不称冷查询。

1024真实小文件及一个子目录，31次同一文件往返rename。每次显式通知Change并推进虚拟时钟，执行重扫、建索引、交换快照和准确新路径查询；旧租约保留至新查询成功，包含两代索引共存。

| 时间（31样本） | p50 ms | p95 ms |
| --- | ---: | ---: |
| rename→显式通知→重建→查询 | 77.7892 | 92.5761 |
| scan/index/publish | 77.1904 | 91.9954 |
| 选择性新路径查询 | 0.0037 | 0.0054 |

整个夹具子进程约2.906秒；GetProcessMemoryInfo峰值工作集10756096 bytes，峰值提交3887104 bytes；5ms采样PrivateUsage最大3858432 bytes，548采样点。口径不同，采样可能漏瞬时峰值，均未测量内核通知对象成本。这些数值仅代表本次小夹具。

虚拟时间绕过成功冷却与失败退避，Windows通知显式模拟；结果不代表 Windows 原生通知延迟，不证明变化 100ms 内可见或百万记录常驻内存 100MB 内。31样本尾部稳定性有限。

Windows复现：

```powershell
cargo +1.99.0 test --release --offline --target-dir target/stage3
python scripts/measure-live-windows.py
```
