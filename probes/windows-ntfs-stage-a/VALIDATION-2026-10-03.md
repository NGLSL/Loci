# 未注入对照与管理员 NTFS 实测

日期按用户时区：2026-10-03，Asia/Shanghai。证据目录名含 UTC 时间，可能显示 2026-10-02。代码起点为独立分支 `codex/windows-ntfs-stage-a` 的 `c0267fdc08cdd76c168556875c331f167ed5a4c7`，主分支保持 `e204917644fb771e5c92e74d130a34127ca8f74e`。本报告只覆盖独立探针，不继承现有共享引擎的 Windows 验收。

**最终结论：未加载 Aura 的管理员环境下，小型真实 NTFS 夹具的文档化 MFT + USN 建库、六项并发操作、库存/游标落盘、跨进程离线恢复和完整路径集合核对均已通过。** 普通 token 的卷级路线仍明确拒绝。通过的是有界混合原型；纯 MFT 的独立命名空间发现、百万级搜索/监听、共享引擎装配及下列未验证事项没有因此完成。

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

## 第三次管理员执行：真实建库与跨进程恢复通过

用户明确授权第三次 UAC；固定代码为 `2661fdfa79037f42630237f43fa5bab9fdbf8815`。release 重新构建，build/capabilities/bootstrap/recover **全部 exit 0**，runner exit 0。两个探针进程均 token elevated=true、envbox module=false；文件变更全部为新工程小夹具，没有 journal/系统设置修改。

| 验收 | 真实结果 | 分类与范围 |
| --- | --- | --- |
| FSCTL_ENUM_USN_DATA 到 EOF | 2162732 条 MFT records，3518 batches，MFT_stream_complete=true | 真正整卷 API 读取；没有对全卷对象去重或校验全卷路径；不是216万文件监听 |
| 范围内存 | retained graph/native objects=19，raw names=640 bytes，directory seeds=7 | 其它名称仅当前64KiB返回批次/解码期间存在，不写盘、不打印 |
| MFT 投影 | missing=0、extra=0、degraded=false、projection_complete=true | 仅当前20项夹具，使用namespace directory seeds与FindFirst/NextFileNameW；不是纯MFT首次发现证明 |
| 并发 journal 操作 | required=6，matched=6；96 read records | CREATE、DELETE、file old/new rename、directory old/new rename均匹配真实object/parent/raw name/reason |
| 初次建库完整状态 | 20 entries，独立全路径集合相等，稳定 cutoff `37743977840` | atomic inventory+cursor创建成功，非count/checksum采样 |
| 进程间离线变化 | bootstrap进程退出后实际增删/文件与目录改名/硬链接增删 | 在原进程不存在时修改，非仅关闭卷句柄模拟离线 |
| 恢复 journal 操作 | required=6，matched=6；29 read records | cursor `37743977840` → `37743980936`；真实新进程读库存和journal |
| 恢复完整状态 | 20 entries，独立全路径集合相等且稳定cutoff；atomic replacement成功 | 数量相同但路径/关系确实变化，不能以20→20替代集合证明 |
| 实际 USN version | ENUM 只接收V2且全部通过；READ trace记录 `{2}` | query公布2..4，真实消费证明仅V2；V3字节测试/V4拒绝边界保留 |

journal ID 在成功建库/恢复中保持 `0x1d9b89b541688b0`。本次能力检查 FirstUsn `37706792960`、NextUsn `37743962936`、LowestValidUsn `0`；建库/恢复分别推进到上表截止点。当前 retained window 没有发生自然回卷或 journal 重建，不能以这些值代替故障证明。

| 原型进程资源 | bootstrap | recover |
| --- | ---: | ---: |
| 完整操作 wall time | 4526 ms | 9 ms |
| 本进程 handles 前 → 后 | 56 → 56 | 56 → 56 |
| 清理后 total working set | 5914624 bytes | 5582848 bytes |
| 清理后 private commit | 1581056 bytes | 1040384 bytes |
| 内核内存 / private working set | 未测 | 未测 |

成功标记没有输出建库/恢复峰值工作集，不能把清理后值称为峰值；前面的6119424 bytes峰值属于第二次预算失败进程。脚本持有的目录pin句柄与探针进程计数分开。4.526秒包含目录种子、整卷枚举、夹具操作、重放、oracle和保存；此前两次枚举可能已预热缓存，cold/warm状态没有控制，没有p50/p95或吞吐承诺。9毫秒只代表当前小夹具的29条重放和完整校验，不是百万索引恢复耗时。

成功私有 evidence：`.scratch/windows-ntfs-stage-a/run/native-20261002T201520Z-08b81dd64d0e4130be69c188c7bf1cac/`。其中有全部原生命令stdout/stderr、原始exit codes、离线变更marker、结果、真实库存文件。前两次失败保持独立目录，未覆写。

### 独立落盘库存交叉核对

另外由第二套Python标准库/ctypes实现，只读解码保存库存并递归遍历同一真实fixture。遍历前先检查raw scope精确等于明确工程测试根，不按库存任意scope读取。库存大小 **1880 bytes**，magic/version/声明长度/FNV checksum通过，journal ID=`0x1d9b89b541688b0`、cursor=`37743980936`；库存的parent/object/name图派生出全部路径，与独立 `os.scandir` 的**完整raw UTF-16路径集合精确相等，20/20**。每个路径另以GetFileInformationByHandle检查object、legacy serial和attributes，全部相等，root identity也匹配。

原始D800保留；ADS不产生条目。旧删除/文件名/目录名/旧硬链接名都不存在，新名称存在。offline source/new link同一native object，库存有恰好两个搜索路径、native link count=2；native-building下outer/renamed-dir inner link也同一对象、link count=2。硬链接USN reason没有单项断言，但最终所有关系与真实文件一致；不能由单个USN FileName假定所有链接路径。

该独立checker使用本机已安装Python的标准库，无安装、无第三方包；Cargo原型和主验证流程不依赖Python。报告之外的私有JSON保存精确核对统计，不打印外部名称。复现入口为同目录 `verify-snapshot.py`，只能对工程run内已声明fixture与scope外库存运行。

## 最终复现入口

先在独立工作树普通终端运行 `verify.ps1 -Volume D:`，普通token的FSCTL拒绝应保留为BLOCKED，不能当空结果。已由操作者提供管理员终端并明确允许D:卷级MFT读取时：

```powershell
Set-Location D:\Project\Loci\.scratch\worktrees\windows-ntfs-stage-a
.\probes\windows-ntfs-stage-a\verify.ps1 -Volume D:
# 新的未加载Aura的管理员终端；这条命令每次新建独立target/evidence/小fixture：
.\probes\windows-ntfs-stage-a\verify-ntfs.ps1 -AllowVolumeEnum
```

普通终端可以显式选择 `-RequestElevation -AllowVolumeEnum` 来请求可见UAC；不传RequestElevation不会自动提升。该runner在恢复阶段自动传 `--verify-offline-fixture-events`，六项真实操作缺失就失败。读取现有成功库存的额外独立checker无需提权或卷枚举。

复核本机已保留的成功夹具（可选Python，使用标准库）：

```powershell
$ntfsEvidenceRoot = 'D:\Project\Loci\.scratch\worktrees\windows-ntfs-stage-a\.scratch\windows-ntfs-stage-a\run\native-20261002T201520Z-08b81dd64d0e4130be69c188c7bf1cac'
python .\probes\windows-ntfs-stage-a\verify-snapshot.py `
  (Join-Path $ntfsEvidenceRoot 'fixture-native-9513fcc2827b4271b806d89534f5c4cc') `
  (Join-Path $ntfsEvidenceRoot 'checkpoint-7f066c945c254b6596ed7eaba4876655/inventory.lcusn') `
  --verify-native-fixture
```

这份checker在本次已有夹具上实际exit 0，全部路径/原生身份/属性与两对硬链接断言通过。脚本可评审、默认只输出统计JSON；不存在工程run时拒绝执行，run自身/祖先及输入路径的literal reparse在resolve前逐层拒绝。它用于受控静止夹具的独立复核，不承诺敌对子对象并发替换的事务隔离。私有JSON为成功evidence中的 `independent-crosscheck-cli-result.json`。

## 保留的验收边界

真实 V3/V4 消费、FRN slot 复用、大小写敏感目录、非 NTFS 卷、更多 reparse 类型、journal 自然回卷/重建、成功 pending USN 取消、断电耐久性、private working set/内核内存、长跑和 100k/1M 真文件吞吐仍待验收。注入故障与真实内核条件分别记录。共享接口建议和分档预算见 [REPORT.md](REPORT.md)，持久格式/CLI/路径保护见 [IMPLEMENTATION.md](IMPLEMENTATION.md)。本轮不改共享接口、GUI、main，不发布或推送。
