# 未注入对照与管理员 NTFS 实测

日期按用户时区：2026-10-03，Asia/Shanghai。证据目录名含 UTC 时间，可能显示 2026-10-02。代码起点为独立分支 `codex/windows-ntfs-stage-a` 的 `c0267fdc08cdd76c168556875c331f167ed5a4c7`，主分支保持 `e204917644fb771e5c92e74d130a34127ca8f74e`。本报告只覆盖独立探针，不继承现有共享引擎的 Windows 验收。

## 普通权限，Aura 解除后的对照

新启动探针报告 `process_envbox_module_loaded=false`、`process_token_elevated=false`。PowerShell 7.6.6；CIM 观测 Windows 11 Education 10.0.26200 x64；Rust 1.99.0 b940084d7 / LLVM 23.1.1 / MSVC。

`verify.ps1 -Volume D:` 完成 fmt、all-targets check、debug/release build，两套各 15 tests、0 failed、0 ignored（包含一个跨进程 helper）。真实小夹具均通过：并发 add/delete/file rename/directory rename 的六条通知、全路径集合、同对象三个硬链接名称、17 项全 scope/2 项子 scope 的目录项身份图、Unicode、孤立 D800、297 UTF-16 单元 extended 长路径、ADS 不作为搜索目录项、symlink 不跟随、20 次空闲取消。

| 小夹具指标 | debug | release |
| --- | ---: | ---: |
| 完整测试耗时 | 47 ms | 44 ms |
| 空闲取消前后 handles | 78 → 78 | 78 → 78 |
| Total working set 前 → 后 | 5468160 → 6119424 bytes | 5328896 → 5660672 bytes |
| Private commit 前 → 后 | 843776 → 1064960 bytes | 856064 → 983040 bytes |
| Peak total working set | 6119424 bytes | 5693440 bytes |

上述耗时包含建夹具、变更、遍历、核对和取消，不是纯枚举吞吐。Private working set、Rust heap 专项及 NTFS 内核开销未测量；不能把 private commit 当 private working set。首次能力轮的进程总 handles 78 → 79，后续两轮 79 → 79；内部每轮 20 次拒绝调用计数稳定，首次额外一个 handle 未归属。

普通 token 的零权限卷打开成功，但 QUERY/ENUM/READ 均返回原始 OS code **1**；GENERIC_READ 卷打开返回 OS code **5**。这与先前加载 Aura 的探针结果相同。故当前证据支持权限/句柄访问模式限制，**不支持把失败归因于 Aura**；不据单次对照宣称 Aura 对所有 API 或性能毫无影响。私有日志：`.scratch/windows-ntfs-stage-a/run/clean-control-6306c498a5f744be84ec6668990c2ddf/ordinary-checks.txt`。

## 首次管理员执行

用户明确授权一次可见 UAC，并允许有界、只读的 D: 整卷 MFT 验证。实际 `verify-ntfs.ps1 -RequestElevation -AllowVolumeEnum` 创建新 evidence、target、工程小夹具，进入管理员子进程；探针报告 `process_envbox_module_loaded=false`、`process_token_elevated=true`。没有创建、删除、调整 USN journal 或修改系统设置。没有记录外部用户文件名。

卷为 D: NTFS，serial `0xf228c46d`，GUID `\\?\Volume{3b1b89c0-c213-495c-b692-f1e2d9987c9d}\`，flags `0x1c72edf`。

| 操作 | 管理员真实结果 | 证据边界 |
| --- | --- | --- |
| CreateFileW zero_access | 成功 | 三个 FSCTL 仍返回 OS code 1；管理员身份不弥补错误的句柄访问模式 |
| CreateFileW GENERIC_READ | 成功 | 文档化路线可打开卷 |
| FSCTL_QUERY_USN_JOURNAL | 成功，三轮一致 | journal ID `0x1d9b89b541688b0`，FirstUsn `37706792960`，NextUsn `37742559832`，LowestValidUsn `0` |
| journal 支持版本 | 返回 major 2..4 | 这是内核公布的支持范围，不能当作 V3/V4 记录已消费；原型 native bootstrap 仍只接受 V2 |
| FSCTL_ENUM_USN_DATA EOF sentinel | OS code 38，成功识别 EOF | 只证明调用可用；真正枚举另由 bootstrap 执行 |
| FSCTL_READ_USN_JOURNAL at NextUsn | 成功，cursor 相同、0 records | 真实非阻塞空尾读取，不证明非空变更重放 |
| 重复调用生命周期 | 两种句柄各 20 次，64 → 64 handles；三轮一致 | 包含当前成功/拒绝调用；不是 pending 成功 I/O 的取消实测 |

每轮能力调用 3/1/1 ms；GENERIC_READ 的 journal 身份/边界在三轮保持一致。第一轮进程总 handles 62 → 63，后续两轮 63 → 63，未追踪首次额外 handle；total working set 5857280 bytes，private commit 999424 bytes。这是能力探测进程，而非建库进程峰值或产品内存。

首次建库失败：`MFT object/name budget exceeded; incomplete, no checkpoint published`，exit 1。这份实现保留整卷对象 map，上限 200000 objects / 32 MiB raw 名称；错误未区分是哪项先达到，不能填写推测条目数。进入此错误分支意味着已实际收到并解析 MFT records；不能把 EOF sentinel 单独当作这项证据。没有完整枚举或一致性通过结论。

失败后实查：checkpoint 目录 0 文件，`inventory.lcusn` 不存在；小型工程夹具共 20 项；`offline-mutations.txt` 和 recover 日志不存在。没有发布完整状态，也没有执行离线恢复。原失败现场保留，不覆盖、清理或伪装为后续成功。

私有 evidence：`.scratch/windows-ntfs-stage-a/run/native-20261002T200052Z-e073114e26f94f5e8cbe574e823ef1fb/`。管理员 runner 的 C# pins 已实际走过目录创建、构建、能力检查、bootstrap 启动及失败清理；离线变更的 pin 覆盖仍须成功路径验收。

## 针对实测失败的实现修正

后续实现改为按批读取整卷 MFT，只保留声明 scope 的必要关系，不在内存保存整卷名称图。范围由小型目录项 inventory 的普通目录身份提供 seed；MFT 记录的 parent/object 匹配 seed 后保留，seed 可被真实 native 记录覆盖。范围外名称仅短暂存在于当前 64 KiB 返回批次及解码对象中，既不写盘也不打印。它是混合路线研究，**没有证明纯 MFT 能独立发现全部 scope 关系**。

依然保留 20 秒枚举、8192 scoped 项、32 层、32 MiB retained 名称预算；额外限制最多 2000000 个 scanned records。扫描预算是退出条件，不是容量/吞吐承诺，也不是创建百万文件。成功/失败只输出 scanned、retained、batch 和字节计数。没有改根 Cargo、共享引擎或其 4096/128 约束。

上述 2000000 是首轮流式实验预算；下面第二次管理员实测触发该预算，之后校准为最多 **10000000** 条扫描记录。20 秒、批次与 retained/scope 限制不变。这是对 D: 已有 MFT 读取工作量的调整，不能据此宣称纯 MFT 建库或百万级索引产品化。

源代码同时补充失败返回后的资源诊断，以及 capability 多轮中任一轮失败不能被最后一轮成功掩盖。新版管理员复验及稳定完整路径结果另按实际执行更新；首份失败不能被这些代码修正冒充为已通过。

范围 seed 在受控 writer 启动前收集，避免读取 seed 时就与自己的删除/改名竞争。MFT 历史路径已消失时，OS code 2/3 明确计数；路径前后对象身份或 serial 不匹配也计数，均令投影 degraded，不把别的对象链接误并入。ACL/其它 I/O/未知版本仍失败。只有最终 namespace + USN + 独立 oracle 路线通过才能发布 hybrid 完整状态，degraded 投影不能声称完整。

另外补齐真实 USN 操作验证：bootstrap 在变更前记录旧对象/parent，变更后记录新增对象；要求 create、delete、文件 old/new rename、目录 old/new rename 六项都在实际 READ 批次中匹配 object、parent、raw UTF-16 name 和 reason mask。`recover --verify-offline-fixture-events` 从保存库存取得旧对象，要求对应六项离线操作命中。缺项不保存。匹配器只保留六项已知工程动作和版本集合，不保留外部名称。硬链接增删仍以最终完整路径集合证明，没有单独核对 USN hard-link reason，也不声称 reason 能还原所有链接名称。

修正版本 fmt、all-targets check、release build、PowerShell AST 解析通过；debug/release 各 **24 tests、0 failed、0 ignored**（包含一个跨进程 helper）。其中新增真实工程文件删除/改名/路径复用测试验证 stale 投影处理；范围/预算、USN 六动作正反、OS5 和未知版本使用明确注入。私有日志为 `run/streamed-trace-{debug,release}-tests.txt`。这些测试不能替代修正后管理员真 USN 的成功路径。

## 第二次管理员执行：真实流式预算校准

用户再次明确授权一次 UAC。固定代码 `76eac4fd477b573ed28445195a004d1258047594` 重新构建并运行，管理员 token=true，Aura module=false；build/capabilities 均 exit 0，bootstrap exit 1。

MFT 批次确实读取/解析 **2000001 条记录、3260 batches**；只保留 19 个 graph/native objects、640 bytes raw 名称、7 个 directory seeds。该计数是读取的记录数，没有执行整卷 dedup，不等于独立文件条目数。第 2000001 条触发扫描预算，`MFT_stream_complete=false`，没有达到卷尾。

错误为 `MFT scanned record budget exceeded; incomplete, no checkpoint published`，本地预算错误没有 OS code。失败清理后测得：elapsed 3826 ms；handles **56 → 56**；total working set 5963776 bytes，private commit 1409024 bytes，peak total working set 6119424 bytes。内核、private working set 仍未测量。它直接支持 bounded user-space 生命周期结论，不能外推成功 USN 的 pending 取消或长期资源。

私有 evidence：`run/native-20261002T201052Z-a1402c7be7a24817ad928200a45c421d/`。此前的失败现场仍保留。尚未运行离线变更/恢复，也没有完整库存。依据该实测，读取工作量预算改为 10000000 records，同时保留20秒枚举及小 scope/批次/内存限制；再次原生执行必须另获 UAC 授权，不能复用已经退出的管理员 token 或此前的一次性许可。

## 保留的验收边界

真实 V3/V4 消费、FRN slot 复用、大小写敏感目录、非 NTFS 卷、更多 reparse 类型、journal 自然回卷/重建、成功 pending USN 取消、断电耐久性、private working set/内核内存、长跑和 100k/1M 真文件吞吐仍待验收。注入故障与真实内核条件分别记录。共享接口建议和分档预算见 [REPORT.md](REPORT.md)，持久格式/CLI/路径保护见 [IMPLEMENTATION.md](IMPLEMENTATION.md)。本轮不改共享接口、GUI、main，不发布或推送。
