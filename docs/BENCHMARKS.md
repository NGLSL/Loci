# 基准方法与边界

数据为确定性生成的文件记录或测试自身创建的小文件；不使用用户文件名或文件内容。百万记录基准无需创建百万真实文件。mmap、相关性TopK和百万条实时更新不是本轮能力。

## Windows有界桥接计时

2026-10-02，Rust1.99.0 release，i5-13490F（10核/16逻辑CPU）、约32GiB RAM、Windows11、NTFS。未固定CPU频率/亲和性、未清OS缓存、未确认具体物理盘映射，因此不称冷查询。

1024真实小文件及一个子目录，31次同一文件往返rename。每次显式通知Change并推进虚拟时钟，执行重扫、建索引、交换快照和准确新路径查询；旧租约保留至新查询成功，包含两代索引共存。

| 时间（31样本） | p50 ms | p95 ms |
| --- | ---: | ---: |
| rename→显式通知→重建→查询 | 77.7892 | 92.5761 |
| scan/index/publish | 77.1904 | 91.9954 |
| 选择性新路径查询 | 0.0037 | 0.0054 |

整个夹具子进程约2.906秒；GetProcessMemoryInfo峰值工作集10756096 bytes，峰值提交3887104 bytes；5ms采样PrivateUsage最大3858432 bytes，548采样点。口径不同，采样可能漏瞬时峰值，均不包含Linux内核watch成本。这些数值仅代表本次小夹具。

虚拟时间绕过成功冷却与失败退避，Windows通知显式模拟；结果不代表inotify/poll的原生延迟，不证明Linux变化100ms内可见或百万记录常驻内存100MB内。31样本尾部稳定性有限。

Windows复现：

```powershell
cargo +1.99.0 test --release --offline --target-dir target/stage3
python scripts/measure-live-windows.py
```

## 历史Linux合成索引测量

上轮分离的查询原型在x86_64 Linux overlayfs、可见9CPU及约9.7GiB RAM中，100k记录构建约0.219s、索引6609638 bytes；1M记录构建约2.110s、索引65389956 bytes，报告构建VmHWM约88797184 bytes。这是一次构建计时，不是重复构建分位数。

百万条完整重复基准在90CPU秒、120秒墙钟和1GiB地址空间预算下终止，未记通过。另用独立Python oracle核对24组count/checksum通过。单次高匹配查询仍慢：ab约647ms、项目约384ms；不能作为p95数据。静态索引测量不能代替监听桥接性能。

本轮Linux overlayfs原生桥接创建1024文件、31次rename，观察62个事件、32次构建；变更→polling→250ms成功冷却→重扫/Index→准确查询，p50 **271.276ms**、p95 **280.938ms**。查询本身p50 **0.004044ms**、p95 **0.025615ms**。5ms采样RSS/HWM峰值 **2715648 bytes（约2.59MiB）**，1635采样点，总墙钟8.618秒，退出0。该测试带512MiB地址空间、60CPU秒、120秒墙钟、16MiB输出、256fd预算；不包含内核watch/slab成本或百万记录索引。

Linux复现：

```sh
bash scripts/validate-linux.sh --kernel-overflow --live-metrics
```

这些原生数字只适用于本次overlayfs小夹具，包含刻意的250ms冷却。不要直接比较Windows工作集/PrivateUsage和LinuxRSS，也不能将overlayfs通过称为所有Linux文件系统实测通过。
