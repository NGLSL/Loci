> 本文记录基础提交7415280的验收。增量分支的新增范围、Windows联合51项结果及Linux待复验入口见[INCREMENTAL.md](INCREMENTAL.md)。

# 验证范围

2026-10-02。完成Windows共享逻辑及x86_64 Linux原生基础验收；以下限制仍然适用。

## 已完成的本地验证

Windows debug与release各32项通过，fmt通过；Linux FFI代码在Windows cargo check中通过类型检查。Windows实际文件增删与rename使用工程内临时夹具，通知显式模拟。初始扫描/构建竞态、旧读者版本一致、预取消/执行中取消、重启、模拟漏事件、失败保留旧结果、事件风暴退避均有回归。

基线CLI使用已有百万条合成记录索引核对12查询×complete/first50，共24组count/checksum与独立Python oracle一致；没有逐一比较完整ID集合，不是百万真实文件或百万条实时更新验证。

## Linux原生验证

上轮独立查询/监听恢复原型在x86_64 Linux、kernel 6.18.44、glibc2.41、**overlayfs** 上debug/release各32项普通测试通过；另行观察到真实IN_Q_OVERFLOW，并恢复清单。此时监听与查询尚未桥接。overlayfs结果不能推广到ext4、Btrfs或网络文件系统。

上轮发现旧新Session短暂重叠，33 watches/1fd变成66/2。当前实现先关闭旧Session再重建，并加入Loci-managed的进程共享预算；最终源码在相同Linux overlayfs环境中debug/release各 **53项普通测试通过**，3项默认ignored；另行真实IN_Q_OVERFLOW到查询恢复与原生端到端测量通过。替换窗口 /proc 实测维持 **33 watches/1fd**，drop归0；跨Session的128-watch、8-fd边界与释放通过内核断言。没有以类型检查代替原生结果。

最终修复了Native错误分支遗漏最新Recovery generation/reasons的问题，并明确无watcher期间的变动通过重扫恢复；新增失败元数据、跨线程旧读者和两代背压、无watcher缺口恢复三项原生回归。真实overflow执行8224轮create/delete后，使用实际观察到loss的同一Recovery驱动Native/Index/Query，准确查询到survivor。

## 复现与资源边界

已具备Rust1.99.0/rustfmt、Bash、timeout、Python3的x86_64 Linux执行：

```sh
bash scripts/validate-linux.sh --kernel-overflow --live-metrics
```

普通测试、真实overflow与原生计时分开记录。overflow只读sysctl，最多10000轮create/delete、10秒生成；没观察到真实事件时输出SKIP，脚本不将SKIP算通过。所有夹具只在工程work子目录，不扫描用户home或整台电脑，清理前校验resolved路径。

原生计时采用1024文件和31次rename；子进程有512MiB地址空间、60CPU秒、120秒墙钟、16MiB输出和256fd预算。软件不安装依赖或修改sysctl。RSS/HWM不包含内核watch/slab对象字节成本。

查询结果携带快照版本及开始/结束校正状态。Validated仅对应最后观察的截止点，文件系统可能随后变更；cancelled表示部分结果。预算、持续写入或读者保留旧代时，查询可返回旧快照并显示待校正；不能称为实时最新。

百万条在线重建、长期压力、其它Linux架构/文件系统、恶意TOCTOU和生产调用方集成尚未完成验证。

低层API约束：收到WatchLost的Session应丢弃并重建，直接复用可能保留过时登记。QueryHandle可以比发布者活得更久，当前没有独立的监控停止状态；调用方必须管理发布者生命周期，不能把停止poll后的快照称为持续监控。
