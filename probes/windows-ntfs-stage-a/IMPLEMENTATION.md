# Windows 阶段 A：持久原型与后续原生验收入口

日期：2026-10-02。**实现已完成，真实 MFT + USN 成功验收仍待用户解除 Aura 后执行。** 这是独立、有界的技术原型，不是已产品化的百万级 NTFS 后端。初轮 OS、NTFS、普通 token 拒绝、小型真实夹具、内存/时间/句柄及官方 API 依据保留在 [REPORT.md](REPORT.md) 和 [API-NOTES.md](API-NOTES.md)。当前环境下的测试不能当作未注入主机的验收。

## 基线与改动范围

- 本机基线 `e204917644fb771e5c92e74d130a34127ca8f74e`；云端参考 `de8f897eda0ba856b1f59e9497f2bd079d287686` 是该提交祖先，没有重置到参考提交。
- 分支 `codex/windows-ntfs-stage-a`，工作树 `D:\Project\Loci\.scratch\worktrees\windows-ntfs-stage-a`；首份独立提交 `61528a899377b7f97851402f1d609d7027132ded`。
- 所有可评审产物在 `probes/windows-ntfs-stage-a/`。未改变根 Cargo、共享 events/storage/engine/index/lib、公共文档、.gitignore 或 main；没有 push、PR、发布或合并。
- Rust 1.99.0 / std-only；Windows FFI 自行声明。管理员复验脚本的目录保护使用 PowerShell 自带 Add-Type 编译内嵌 C# P/Invoke，不添加 Cargo 依赖，不安装全局软件。
- 常规 target、日志和夹具留在该工作树 `.scratch/windows-ntfs-stage-a/`；管理员 runner 每次在自己的唯一 evidence 目录中新建独立 target，避免复用旧构建目录的重定向。它们不纳入提交。

## 完成的代码

`src/snapshot.rs` 将完整小型库存与游标存到同一个 `inventory.lcusn` 文件：volume serial、journal ID、cursor、scope root 对象身份、raw UTF-16 scope，以及所有 `(parent, object, raw_name)` 目录项和属性。一个对象允许多条硬链接关系；不以完整路径作唯一对象主键。此关系三元组不是最终共享 ADR 的稳定 opaque EntryId。

格式自有 magic/version/长度/校验；上限 2 MiB、8192 条目录项、32768 单元 scope、255 单元名称。解码明确拒绝损坏、未知版本/记录、重复关系、同路径碰撞、父对象缺失、非目录或 reparse 父、目录循环、多父目录和冲突属性。保留孤立 UTF-16 surrogate 和完整 u128 身份。校验用于检测损坏，不提供恶意篡改认证。当前 serial 仍是 legacy 32-bit 值，GUID 和能力另由探针观测；这不是防克隆的产品来源身份格式。

库存和 cursor 先在同目录 create_new 临时文件中写入并 sync_all，再以 MoveFileExW 发布。`bootstrap` 使用**不替换已有目标**的原子创建，消除“先检查不存在再覆盖”的并发窗口；`recover` 使用原子替换。失败返回 OS code，保留上一份成功库存；不会提前单独推进 cursor。没有断电耐久性或多进程并发 recover 协调承诺；同一 storage 的恢复操作要求串行执行。

旧 `ntfs` 单进程命令已删除，避免只保存 cursor、依赖进程内旧库存的路径与真正的跨进程恢复混淆。最终入口为：

| 命令 | 行为和成功门槛 |
| --- | --- |
| `capabilities D:` / `capabilities-repeat D:` | 只读卷/API/token/模块探测；ENUM 使用 EOF sentinel，不读取全卷文件记录。拒绝返回非零，不能当空索引 |
| `fixture <run-root>` | 小型目录扫描 + ReadDirectoryChangesW、身份/硬链接/raw 名称/ADS/reparse 和取消夹具；不是 USN 成功证据 |
| `bootstrap <fixture> <store> --allow-volume-enum` | 记录开始 journal 边界，工程内小型并发变更，有界整卷 MFT，补齐 scoped namespace，USN 重放，独立完整路径集合及 journal 截止点核对，原子创建库存 |
| `recover <fixture> <store>` | 新进程读取库存，核对固定 scope/root/volume/journal 身份和 retention，再处理离线 journal；完整路径集合与稳定截止点相等后原子替换；失败保留旧库存 |

`bootstrap` 的小夹具包含并发 add/delete/file rename/directory rename、两个硬链接名称、Unicode、孤立 D800、extended 长路径和 ADS。`verify-ntfs.ps1` 在 bootstrap **进程退出后**实施真实离线增删、文件/目录改名及硬链接增删，然后启动独立 recover 进程。只有两个 native 命令都返回 `native_complete=true` 才记录整条小夹具通过。

这里的 `native_complete=true backend=hybrid` 仅指**小 scope 的混合路线**：MFT 投影是单独统计项，完整关系来自 scoped 目录枚举；任意卷事件都触发小 scope 的保守重扫。因此 `mft_projection_complete=false` 可以与混合路线通过同时出现，绝不能把后一项当作纯 MFT 路径完整性或百万级增量性能通过。所有独立路径 oracle 都比较完整 raw UTF-16 集合，不是 count/checksum/前 N 条。hardlink MFT 代表名落在 scope 外仍须未来取得内核实例证据。

成功状态表示 journal cursor 所代表的已验证截止点；之后出现的新变化由后续同步处理。持续卷写入可能让预算内无法取得稳定截止点，此时拒绝发布。扫描与重放期间固定的 root、scope、serial、journal identity 不得因重新读取命名空间而改变，变化明确要求重建。

## 路径和生命周期保护

Rust 和管理员 runner 都使用普通目录句柄，从盘根到工程 run、fixture、storage/evidence 按祖先顺序持有：FILE_LIST_DIRECTORY=1，SHARE_READ|SHARE_WRITE，**不共享 DELETE**，BACKUP_SEMANTICS|OPEN_REPARSE_POINT，读取已打开对象属性并拒绝 reparse 祖先。Rust pin 覆盖一次 bootstrap/recover；PowerShell pin 另覆盖整个 runner，包括子进程间的离线变更、日志和独立 target。

实际小型测试发现 desired access=0 的句柄未能阻止目录改名，改为 access=1 后，root/store 的真实改名均返回 OS code 32；期间子文件原子替换仍可成功。这是当前注入环境下的真实文件操作证据，不是仅源码推断。脚本内 C# pin 的管理员完整生命周期及并发 junction 攻击没有实际运行，仍列入复验项。scope 内所有子对象的敌对并发重定向、文件 ACL 攻击和事务隔离不在该小夹具证明范围，不能以目录 pin 宣称任意敌对命名空间安全。

I/O 每批 64 KiB；MFT 最多整卷 200000 对象、32 MiB raw 名称、20 秒枚举预算；scope 最多 8192 项/32 层，重放最多 100000 records/10 秒。D: 的非夹具对象也占用预算。2 秒 I/O 到期是请求 CancelIoEx 的时限，仍须等待最终完成再释放 OVERLAPPED/buffer/event；不是硬停止 SLA。各正常/错误路径 RAII 释放句柄；成功 USN pending I/O 取消仍待实测。新 runner 额外持有目录 pins，资源统计需将脚本句柄、探针用户空间、内核缓存分开。

## 当前已执行与待执行

当前进程观测仍是 Windows 11 Education build 26200、D: NTFS、非提升 token，探针加载 envbox-runtime64.dll。以前的零权限 FSCTL 返回 OS code 1、GENERIC_READ 卷打开返回 OS code 5；没有证据可将这些失败归因于 Aura，也没有管理员成功对照。用户最新决定：先完成实现，解除 Aura 后再做相关验证。本轮没有 UAC、journal 创建/删除/调整、系统设置修改或全卷数据枚举。

最新 debug/release 各 15 tests、0 failed、0 ignored，通过离线构建及格式检查。15 项中包含一个子进程 helper，不能解读为 15 项独立 NTFS 原生验收。新增真实磁盘/进程测试为：

- 真正的目录项库存（含硬链接和孤立 surrogate）写盘后，新测试进程重新加载，与独立完整路径 oracle 相等。
- 拒绝再次原子创建已有库存，旧字节完全保留；root/storage 的真实改名被句柄 pin 拒绝，持有 pin 时子文件替换仍成功。
- 原进程退出后真实增加文件，旧库存集合确实落后；当前权限下 recover 拒绝，旧库存字节保持不变。

最终两套日志均记录：首次建库覆盖拒绝 OS code **183**；storage/root 改名拒绝 OS code **32**；recover 的卷打开拒绝 OS code **5 / PermissionDenied**。日志保留在本地 `.scratch/windows-ntfs-stage-a/run/durable-{debug,release}-tests.txt`，不提交。恢复失败不能标为已重放离线变化。

以上测试的文件/root/volume identity 是真实值，但 journal ID=u64::MAX、cursor=0 **是显式注入值**。所以它们证明库存跨进程保存和失败保护，**不证明真实 USN 离线变化已经重放成功**。格式损坏、journal 回卷/重建/游标失效、来源切换和 V3 记录也是字节/边界注入，不冒充内核故障。真实 FRN slot 复用、大小写敏感目录、ReFS/非 NTFS 卷、成功 USN 生命周期和管理员 runner 仍未验证。

`verify-ntfs.ps1` 已通过 PowerShell AST 语法解析；当前普通 token 无参数调用实际退出 20，在创建 evidence/target/fixture 之前拒绝，没有提权/枚举。脚本内 C# 保护代码已保留可评审，但其管理员执行没有用这一拒绝测试代替。

第一份 REPORT 中 47/45 ms 小夹具及 handles、total working set、private commit 数据属于首份版本的真实测量，保留历史归属；本次没有重新制造全卷性能数字。private working set、NTFS 内核开销、长期稳定性仍未测量。一次早期 checkpoint OS code 5 的未知根因也保留在 REPORT，不声称已定位 Aura。

## 解除 Aura 后的复验命令

先在**没有经 Aura 启动的新终端**检查探针自身模块标记；关闭 Aura 界面未必会卸载已注入进程中的模块。普通 token 对照和管理员 token 对照分别保留，不能假定 RunAs 就一定是未注入进程。

普通终端的小型检查（不执行整卷 MFT）：

```powershell
Set-Location D:\Project\Loci\.scratch\worktrees\windows-ntfs-stage-a
.\probes\windows-ntfs-stage-a\verify.ps1 -Volume D:
```

新开的管理员终端先运行能力检查；不传 AllowVolumeEnum 时只有只读能力探测、构建和独立证据目录，没有建库：

```powershell
.\probes\windows-ntfs-stage-a\verify-ntfs.ps1
```

只有操作者明确选择 D: 的有界**整卷 MFT 读取**后运行：

```powershell
.\probes\windows-ntfs-stage-a\verify-ntfs.ps1 -AllowVolumeEnum
```

每次都是新 evidence、新 target、新工程小夹具；不会记录其它用户文件名。既有 journal 不可读/不活动、权限拒绝、版本不支持、全卷预算超限或稳定截止点失败都返回明确失败，留存证据，不自动创建 journal 或发布完整库存。journal 不活动时停止报告限制。

可选 `-RequestElevation` 仅在操作者明确选择时触发一次 Windows UAC；当前会话没有调用。运行于已提升终端不需要它。无需全局安装或根项目依赖变更。CLI 的 `bootstrap` 与 `recover` 可分别调用，对应上述表格；fixture/store 必须已存在，位于本工作树工程 run 中，fixture 名须以 fixture- 开头、store 在 scope 外。复验前不自动删除旧夹具或测试日志。

## 共享接口与下一阶段

保留首份 REPORT 的建议：来源身份独立于盘符；对象身份保留 FRN sequence/完整 ID；多条 EntryId 关联 parent/object/raw name；原始名称与匹配策略分开；平台 cursor 用变体，Linux 不强套 USN；连续性、Partial/Unavailable/NeedsRebuild/Stopped 与 OS code 必须显式；inventory 与 cursor 同事务提交。新私有 snapshot 是这一事务要求的小型验证实现，不是共享接口或最终存储格式。等待共享 ADR 后再装配引擎。

下一阶段仍先以小型真实 NTFS 夹具通过 MFT/USN/跨进程恢复，再校准 1k/10k，最后明确授权 100k/1M 真条目。预算方案沿用 REPORT：100k 预留 2 GiB 磁盘、30 分钟/1 GiB private commit 退出；1M 预留 10 GiB、2 小时/2 GiB private commit 退出，按校准调整。要求各档独立完整路径集合、cold/warm 多轮、在线与离线变更、lag/资源/停止证据。数字是执行预算而非性能承诺；当前 200k 整卷与 8192 scoped 原型上限不能覆盖这些档位，也不能通过调高旧共享引擎常量宣称产品化。本轮没有创建十万/百万文件或整盘压力。
