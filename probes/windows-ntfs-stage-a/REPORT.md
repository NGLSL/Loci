# Windows 阶段 A：NTFS / USN 技术原型实测

日期：2026-10-02。结论：**独立探针、普通 token 的拒绝路径、真实小型 NTFS 命名空间/降级夹具已交付；文档化 MFT + USN 成功路线被当前权限阻断，阶段 A 的 NTFS 完整验收尚未通过。**

不能把本报告中的目录扫描、ReadDirectoryChangesW 或记录字节注入测试当作 FSCTL 成功，也不能继承旧 Windows 引擎验收来填补本轮缺口。

## 基线、隔离与产物

- 本机起点：`e204917644fb771e5c92e74d130a34127ca8f74e`，`main`，工作区干净。
- 用户给出的云端参考：`de8f897eda0ba856b1f59e9497f2bd079d287686`，已核实是本机起点的祖先，没有 reset。
- 独立分支：`codex/windows-ntfs-stage-a`。
- 独立工作树：`D:\Project\Loci\.scratch\worktrees\windows-ntfs-stage-a`；创建时只有原主工作树，没有复用其他工作树。
- 交付范围：本目录的 Cargo 项目、源文件、`verify.ps1`、本报告及 [API-NOTES.md](API-NOTES.md)。Rust 1.99.0，std-only，无 Cargo 依赖，自含 `[workspace]`。
- target 与全部最终测试根：工作树内 `.scratch/windows-ntfs-stage-a/{target,run}`，不纳入提交。夹具保留，不自动清理用户目录。
- 没有修改根 Cargo、共享 events/storage/engine/index/lib、公共文档、`.gitignore`；没有 GUI、发布、push 或合入 main。

## OS、卷与权限：本次直接观测

| 项目 | 结果 | 证据范围 |
| --- | --- | --- |
| OS | Windows 11 教育版，10.0.26200，x64 | 本机会话的 CIM / OS API 观测 |
| Rust | 1.99.0 `b940084d7`，x86_64-pc-windows-msvc，LLVM 23.1.1 | `rustc -Vv` |
| 文件系统 | D: NTFS | `GetVolumeInformationW`，真实夹具文件操作 |
| 卷 serial | `0xf228c46d` | Win32 legacy 32-bit volume serial，本原型存为 u64；不是全局唯一身份承诺 |
| 卷 GUID | `\\?\Volume{3b1b89c0-c213-495c-b692-f1e2d9987c9d}\` | `GetVolumeNameForVolumeMountPointW` |
| 卷 flags | `0x1c72edf` | 能力 flags 不表示 journal 已活动或可读 |
| 当前 token | `TokenElevation = false`，有效管理员角色检查 false | 普通/未提升 token 的实际观测；没有运行管理员对照 |
| 运行时环境 | 探针自身 `GetModuleHandleW` 检测 `envbox-runtime64.dll` 已加载 | 注入进程观测，不是未注入物理主机对照；没有验证该运行时是否影响 FSCTL |
| journal ID、First/Next/Lowest USN、内核记录版本范围 | **未取得** | Query 拒绝，不能填写猜测值 |

### 文档化路线的真实拒绝结果

debug 和 release 各在同一进程重复 3 轮，每轮还有 20 次有界错误调用；结果一致。

| 操作 | 真实结果 | 可以得出的结论 |
| --- | --- | --- |
| `CreateFileW(\\.\D:, access=0)` | 成功 | 元数据卷句柄可以取得，不表示 Journal 可用 |
| `FSCTL_QUERY_USN_JOURNAL`，零访问句柄 | OS code **1**，`ERROR_INVALID_FUNCTION` | 当前调用失败；不能断言物理 D: 不支持 USN |
| `FSCTL_ENUM_USN_DATA`，零访问句柄，`StartFileReferenceNumber=u64::MAX` | OS code **1** | EOF sentinel 能力探测失败，没有进行 MFT 内容枚举 |
| `FSCTL_READ_USN_JOURNAL`，零访问句柄，query 不可用后的 ID=0 sentinel | OS code **1** | 仅拒绝路径，不是有效 cursor 的 USN 读取证明 |
| `CreateFileW(\\.\D:, GENERIC_READ)` | OS code **5**，`ERROR_ACCESS_DENIED` | 当前 token 无法打开成功路线所用卷句柄 |
| 显式 `ntfs <fixture> --allow-volume-enum` | 退出 **1**，raw OS code **5** | 在创建变动子根之前失败；fixture 根直接条目数 9 → 9，没有运行卷枚举 |

微软公开文档要求 change-journal 查询及相关操作具有系统管理员权限。这是文档约束；本次实测只覆盖上面的未提升 token，**没有实测管理员可成功**。[Using the Change Journal Identifier](https://learn.microsoft.com/en-us/windows/win32/fileio/using-the-change-journal-identifier)

没有提权，没有调用 CREATE/DELETE_USN_JOURNAL，没有调 journal 容量、系统设置或目录大小写 flag；没有默认全卷枚举、个人目录扫描或用户文件名日志。可选 unprivileged 控制码只记录在 API 笔记中，没有把非正式语义的备选路径当作已验证替代。

## 原型代码及一致性边界

| 文件 | 责任 |
| --- | --- |
| `src/win.rs` | 只读卷能力、query/enum/read、有界 64 KiB DeviceIoControl、卷/文件身份、硬链接全名称、进程内存/句柄与 token 诊断 |
| `src/model.rs` | 严格 V2/V3 USN 记录解析、完整 FRN/parent、raw UTF-16；V4/未知版本明确 Unsupported |
| `src/checkpoint.rs` | 固定 48 字节检查点，卷 serial + journal ID + cursor、版本/校验、原子替换及失效拒绝 |
| `src/sync.rs` | 显式 opt-in 的有界 MFT 投影、目录项身份图补齐、USN 触发 scoped reconciliation、独立路径 oracle、发布前再验 identity/cutoff |
| `src/fallback.rs` | 独立 ReadDirectoryChangesW / OVERLAPPED / CancelIoEx 探针，无共享引擎依赖 |
| `src/fixture.rs` | 真实小型变动、名称与硬链接验证、完整集合 oracle、原生取消和句柄核对 |

### 建库与重放代码，尚未获成功权限执行

`ntfs` 命令的预期路径是：记录开始 journal ID + NextUsn → 并发工程夹具变动 → MFT 枚举 → 等待 writer 结束 → namespace 补齐 → 重放到当前边界 → 独立完整 raw UTF-16 路径集合相等 → 再 query 验证 journal identity/retention/cursor → 保存检查点。失败、截断、未知版本、无进展、预算超限或截止点变化，均不输出完整 NTFS 成功，也不更新成功检查点。

**这是保守的混合原型。** 单次 MFT/USN 名称不保证全部硬链接，而且代表名称可能在 scope 外、另一个链接在 scope 内。因此初始 MFT 投影另行输出 missing/extra 计数，完整目录项由 scoped namespace 读取补齐。增量遇到任意卷事件时，对小 scope 重新读取目录项身份图，不是 O(delta) 的产品增量实现；卷持续写入时可能拒绝完成，返回预算/截止点失败。这条路线用于研究一致性，不能声称 MFT 纯枚举完整性或 Everything 速度已证明。

目录项候选以 `EntryId(parent, object, raw_name)` 构造，完整路径作为派生值；oracle 另用递归 `read_dir` + `symlink_metadata` 仅生成完整路径集合，不读取待测身份图或 USN。不是 count、checksum 或前 50 比较。三元组是本原型的关系键，rename 后会变化；共享 ADR 仍需决定稳定的 opaque EntryId 与 rename 关联契约。

MFT bootstrap 当前最多 200,000 个**整卷**对象、32 MiB 原始名称、每批 64 KiB、单次 I/O 到期 2 秒请求取消、枚举预算 20 秒；scope 最多 8192 项/32 层、重放最多 100,000 records/10 秒。不调现有共享引擎上限。MFT map 元数据和分配器开销另计，因此 32 MiB 不是进程内存上限。D: 既有对象也计入整卷预算，不能因夹具小就声称此预算足够。

本原型纯解析支持 V2/V3；native bootstrap 只接受 NTFS V2/legacy FRN；V3 是字节注入测试，V4 是无名称的 extent record，明确拒绝。真实 record/version negotiation 尚未验证。[USN_RECORD_V3](https://learn.microsoft.com/en-us/windows/win32/api/winioctl/ns-winioctl-usn_record_v3)、[USN_RECORD_V4](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/ntifs/ns-ntifs-usn_record_v4)

## 真实 NTFS 夹具与注入测试

最终 `verify.ps1` 成功完成，fmt、all-targets check、debug/release build 通过，debug/release 各 **8 passed、0 failed、0 ignored**。以下命名空间夹具也各执行一次，不包含百万合成记录。

| 项目 | 结果及实际证据 | 分类 |
| --- | --- | --- |
| 并发 add/delete/file rename/directory rename | writer 线程真实操作，要求 6 条对应 native 通知，期间各 9 次并发目录遍历；结束后完整路径集合精确等于预期，4 → 4 项且集合变化 | 真实 scan + ReadDirectoryChangesW；**不是 USN replay** |
| 目录项/对象分离 | 17 项全 scope、2 项子 scope，身份图派生路径分别与独立完整路径集合/预期相等 | 真实 NTFS identity namespace |
| 硬链接 | 同一对象 3 个名称，2 个在父 scope、1 个在子 scope；native link count=3，FindFirst/NextFileNameW 完整名称集合精确匹配；子 scope 仍找到内部链接 | 真实文件/名称 API；**MFT 代表名称选择未验证** |
| Unicode | 中文、é、emoji 文件创建、目录返回及完整集合核对通过 | 真实目录 API |
| 原始 UTF-16 | 包含孤立 `0xD800` 的文件创建和原始名称完整集合通过；解析器不做 lossy 转码 | 真实名称 + 解析器字节注入；无共享查询接入 |
| 长路径 | extended 路径 297 UTF-16 code units，真实创建/遍历/集合通过 | 真实目录 API；未覆盖所有路径前缀/长度极限 |
| ADS | 真实 named stream 写入/读回，搜索目录项集合没有增加 stream 项 | 真实文件/目录 API；USN stream reason 未验证 |
| reparse | 工程内目录 symlink 创建成功，保留 link 本身，不遍历目标重复路径 | 真实 symlink；junction/mount/cloud reparse 未验证，native bootstrap 当前拒绝 scope reparse |
| 大小写敏感目录 | 没有启用/调整 flag | 未验证 |
| 身份复用 | V2 完整 64-bit FRN、V3 128-bit identity 不截断；多个链接用不同关系键 | 保留规则/字节测试；真实 MFT slot 复用未验证 |
| checkpoint 磁盘 | 保存、替换、加载、损坏/长度拒绝；真实 share_mode(0) 文件锁使替换失败并保留旧值 | 真实 Windows 文件 I/O，journal 数据是测试值 |
| journal 回卷/重建/cursor 失效 | 注入 first/lowest/next、journal ID 和 volume serial；要求重建，不能恢复完整状态 | **模拟值**，没有触发真实内核 journal 故障 |
| USN 离线恢复 | 代码包含保存、关闭卷句柄、离线增删改名、重开并重放；当前在权限检查处失败 | **阻断**，无真实成功记录；不是跨进程持久完整索引 |

checkpoint 单测曾在早期源码目录测试根首次保存出现一次 OS code 5。最终测试根统一迁入工程 `.scratch`，该失败现场保留在本地 run 下；最终 debug/release 两套检查和额外 3 次 release 定向复验均通过。**根因未确定，不能把路径迁移描述为已证明修复，也没有添加自动重试掩盖错误。** 生产保存仍返回原始 OS error；本轮没有长跑或断电耐久性结论。

## 资源与生命周期实测

以下是最终整条复现脚本的单次小夹具测量，包含文件创建、遍历、验证及 20 次取消，不是纯枚举吞吐，也不能外推到十万/百万。

| 指标 | debug | release |
| --- | ---: | ---: |
| 夹具总时间 | 47 ms | 45 ms |
| 20 次无新事件打开/CancelIoEx/重复 stop/drop 前后 handles | 137 → 137 | 137 → 137 |
| Total working set 前 → 后 | 8,941,568 → 9,355,264 bytes | 8,814,592 → 9,146,368 bytes |
| Private commit 前 → 后 | 2,162,688 → 2,301,952 bytes | 2,170,880 → 2,396,160 bytes |
| Peak total working set | 9,371,648 bytes | 9,146,368 bytes |
| 应用 I/O buffer | 每个 watch 64 KiB | 每个 watch 64 KiB |

FSCTL 拒绝路径每轮 20 次 query/enum/read，持有卷句柄时 139 → 139 handles。首次整轮 137 → 138，随后两轮 138 → 138；首次额外一个进程 handle 的具体归属未追踪，不强称总增量为零。相同拒绝码与后续稳定计数支持有界重复调用的清理，但不证明成功 USN I/O 的取消生命周期。

Private working set、Rust heap 专项与 NTFS 内核 journal/cache 字节**未测量**。Private commit 与 total working set 是不同指标；进程存在 EnvBox 模块，以上不是纯未注入探针的内存成本。

OVERLAPPED、buffer 与 event 在完成前保持存活。到期调用 CancelIoEx 后仍等待最终完成再释放，这是安全生命周期要求；**2 秒是取消请求时限，不保证 2 秒内停止**，驱动停滞可能让完成等待无界。若产品要求硬停止时限，需要隔离 worker 进程，不能提前释放 kernel 正在使用的内存。[Canceling Pending I/O Operations](https://learn.microsoft.com/en-us/windows/win32/fileio/canceling-pending-i-o-operations)

## 无权限 / 非 NTFS 降级

建议复用已有目录扫描 + ReadDirectoryChangesW，而不是把失败当空搜索结果。本轮独立 fallback 夹具验证了这条路径的基础正确性，但没有修改原引擎或放大它的 4096 项/128 目录预算。

| 维度 | NTFS / USN 目标 | scan + ReadDirectoryChangesW |
| --- | --- | --- |
| 首次建库 | 卷 MFT 批量对象枚举，额外命名空间读取补齐所有链接 | 遍历可访问目录，成本与条目/目录和文件系统读取有关 |
| 离线变化 | identity/cursor 保留有效时读取 journal 差异 | 通知不保留离线历史，重开必须扫描校正 |
| 增量连续性 | journal ID、retention、record version/gap 明确校验 | 安装监听后扫描并排空；overflow/gap 必须重扫，不能连续性假定 |
| 权限/覆盖 | 普通 token 的卷访问受限；不能据卷记录泄露不可访问路径 | 仅覆盖 caller 可访问的 root，ACL 拒绝应明确 partial/error |
| 名称/链接 | 对象记录不等于全部 directory entries | 每条目录项天然是一个可搜索路径，可用原始 UTF-16 |
| 性能结论 | 本轮没有可用 USN 成功吞吐 | 只有小夹具时间，不能宣称达到 Everything 级别 |

非 NTFS 的实际卷本轮未选取；代码检查 filesystem name 后明确 Unsupported，生产层可据能力选择降级。网络卷、FAT/exFAT/ReFS、权限不全目录、长跑/overflow 及其它 reparse 尚未作为本轮新原生验收。

## 共享接口需求：等待 ADR，不装配主引擎

1. 来源身份：卷 GUID + 文件系统/serial/能力及所选 scope 身份；盘符只是发现入口。本原型 32-bit legacy serial checkpoint 不是防克隆/冲突的产品格式。
2. 对象身份：来源命名空间内保留完整 NTFS FRN（含 sequence）或 128-bit ID，复用要产生不同对象代际；不能把 MFT slot 或完整路径作为唯一对象键。
3. 目录项身份：单个对象允许多个 opaque EntryId；独立保存 parent ObjectId + raw name，明确 rename 是否保留 EntryId、hardlink 增删/关联的契约。
4. 原始名称与匹配策略分开：Windows UTF-16 code units，不 lossy/不原名 lowercase；父目录大小写规则、ADS 与 reparse policy 是显式能力/策略。
5. 平台游标为变体：Windows `{journal_id, usn, accepted_versions}`；Linux 可为其自身来源代际/位置，不能强加 USN 字段。
6. 有界来源批次：对象变化只作为 reconciliation hints，连续性/gap/record version 必须显式表达；统一 `Unavailable/NeedsRebuild/Partial/ValidatedAtCutoff/Stopped`，OS code 不能丢。
7. 完整索引 checkpoint 需要将 object/entry 状态、scope/policy version 与 cursor 同事务提交。本轮 48 字节文件只有 provenance/cursor，**不能独立恢复未持久化的百万级索引**。
8. 保留现有旧查询/恢复回归，未来装配新增后端再验证；本轮没有改 public trait 或要求 Linux 伪造 NTFS 语义。

## 下一阶段真实 100k / 1M 预算（计划，未执行）

前置门槛：用户选择一个具备权限且已有活动 journal 的 NTFS 测试卷，先在小夹具完成真正 query/enum/read 与稳定完整集合验收，补管理员/未注入对照、跨 scope 代表名称及身份复用；journal 不活动时停止报告，不自动创建。没有这些门槛，不进入大规模。

| 档位 | 数据与磁盘预算 | 时间/进程资源预算 | 退出条件及证据 |
| --- | --- | --- | --- |
| 校准 1k / 10k | 明确工程 root，真实 1k 再 10k entries；预留 1 GiB，测实际每条 NTFS 元数据/USN 量 | 每档最多 10 分钟；记录真实峰值、handles、journal lag | 每档独立完整 raw path set；任何拒绝、gap、超预算或身份变化停止 |
| 100k | 至少 100,000 真实 directory entries，文件/目录/硬链接分别计数；暂预留 2 GiB，按校准数据调整 | 最多 30 分钟、测量并在用户空间 private commit 1 GiB 时停止；单 writer，低并发，批次有界 | 建库/在线变化/离线恢复/stop 全套；cold/warm 至少 5 次给 p50/p95；完整集合差分，不只 checksum/count |
| 1M | 至少 1,000,000 真实 entries，专用既有测试卷优先；暂预留 10 GiB；先估算 journal 可保留窗口 | 最多 2 小时，private commit 暂设 2 GiB 退出预算；先 100k 校准再确认 | 完整集合可按确定性 shard 逐项比较，必须覆盖所有项；记录 lag、rebuild、所有遗漏/额外项数量但不记录个人名 |

这些数字是安全执行预算和讨论起点，不是已测性能/容量承诺。当前原型全卷 200k 对象限制与 scoped 8192 限制不支持上述档位；下一阶段需要专用 NTFS 命名空间/索引实现和预算设计，不能只把共享旧引擎常量调大。卷上其它对象计入 MFT 成本；只能在明确授权范围内测量，不能默认扫描个人目录。没有自动创建 100k/1M 文件、挂载新卷、修改 journal 或整盘压力。

## 可复现命令

以下都在独立工作树运行；脚本只做能力探测和小 fixture，**不会运行整卷 MFT 枚举**。

```powershell
Set-Location D:\Project\Loci\.scratch\worktrees\windows-ntfs-stage-a
.\probes\windows-ntfs-stage-a\verify.ps1 -Volume D:
```

单独执行权限检查（未提升 token 预期退出 1）：

```powershell
.\.scratch\windows-ntfs-stage-a\target\release\loci-ntfs-stage-a.exe capabilities-repeat D:
```

只有选择既有 fixture、明确允许读取整个 D: 的 MFT 且提供可用管理员环境后，才执行下一条。路径必须位于此探针工程 run 内并命名为 `fixture-*`，无权限时在写入前退出。MFT 读取是卷级，retention/cutoff 成功不代表访问任意不可访问路径的产品授权。

```powershell
# 在权限和卷级读取范围已经由操作者确认后，创建一个新的无 reparse 工程小根：
$runRoot = 'D:\Project\Loci\.scratch\worktrees\windows-ntfs-stage-a\.scratch\windows-ntfs-stage-a\run'
$fixtureRoot = Join-Path $runRoot ('fixture-usn-ready-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $fixtureRoot | Out-Null
.\.scratch\windows-ntfs-stage-a\target\release\loci-ntfs-stage-a.exe ntfs $fixtureRoot --allow-volume-enum
```

测试 scope 含 reparse 时 native bootstrap 当前明确 Unsupported；用于未来成功 USN 重跑的 fixture 应选择不含 reparse 的专用小根，仍不得修改 journal。重复 native 命令的变动子根名称含 PID，若已有同名目录会安全失败，需选择新的 owned fixture，而不是删用户内容。

最终脚本的原始小型日志只保留在本地 `.scratch/windows-ntfs-stage-a/run/{capabilities,fixture}-{debug,release}.txt`，不加入提交。`capability_exit=1` 与脚本末尾“小夹具检查完成”同时成立；不能把脚本 exit=0 当 NTFS 主线已通过。

## 待完成验收

- 管理员 token 的 QUERY/ENUM/READ 成功、真实 journal ID 与 record versions；未注入环境对照。
- 真实 MFT 建库 + 并发操作 + USN 重放到截止点的完整路径一致性。
- 真实离线 USN 恢复、跨进程完整索引持久化；回卷/重建不主动破坏卷，只能等待自然条件或使用注入并保持证据分类。
- MFT hardlink 代表名称在范围外的内核实例、真实身份复用、大小写敏感目录及更多 reparse 类型。
- 成功 USN pending I/O 的原生取消/停止、硬停止时限、内核内存、资源长期稳定性。
- 早期一次 checkpoint 保存 OS code 5 的来源、断电耐久性、100k/1M 真文件性能与产品索引装配。
