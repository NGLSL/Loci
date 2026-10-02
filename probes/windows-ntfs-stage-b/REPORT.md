# Windows NTFS 阶段 B：独立后端闭环与真实文件验收

报告日期：2026-10-03（Asia/Shanghai）；原始证据目录使用 UTC 时间。

## 基线、范围与当前状态

主工作树基线 `e204917644fb771e5c92e74d130a34127ca8f74e`；云端参考 `de8f897eda0ba856b1f59e9497f2bd079d287686` 为祖先，没有自动重置。阶段 A 独立提交 `381bb8e30512b447c0b860225233ca51e29ee0e2`。阶段 B 初版代码提交 `7cdf3e41a494d45021ffb2aded10d3154194a0a8`、首次报告 `ce89c1f3110c5b446801e5b4a79e8c27425b67c3`、真实路径竞争和目录句柄定位修正 `34312267b6710c180c3a2b9808e4797b2afbe483`、夹具固定和原生 reference 位宽边界修正 `7f2306fbeb7ea98cb9f2be7eefc1fa84e7a71008`，分支 `codex/windows-ntfs-stage-b`；报告与完成记录单独提交。

独立工作树 `D:\Project\Loci\.scratch\worktrees\windows-ntfs-stage-b`，独立 Cargo 项目、独立 target，Rust 1.99.0、std-only。根 Cargo、共享 events/storage/engine/index/lib 和阶段 A 源码保持原样；没有 GUI、主引擎装配、PR、推送、合并或发布。共享 ADR 为 proposed，不能视为 Linux／共享核心已同意。

本轮交付的是有边界的私有 Windows 后端，初次命名空间发现仍使用指定工程 root 的目录枚举，后续使用 USN 定向校正。阶段 A 已证明 MFT API 可读与有界流式枚举，本轮不重新整卷枚举，不把混合建库称为纯 MFT 全卷产品后端。查询是原始 UTF-16、大小写敏感的相对路径字面子串；原共享引擎的 AND／ext 查询语义未修改。

## 可复现命令

在本工作树执行：

```powershell
cargo test --offline --locked --manifest-path probes/windows-ntfs-stage-b/Cargo.toml --target-dir .scratch/windows-ntfs-stage-b/target
pwsh -NoProfile -File probes/windows-ntfs-stage-b/verify.ps1
# 仅在明确授权本轮一次 UAC 后运行；已是管理员时不弹 UAC。
pwsh -NoProfile -File probes/windows-ntfs-stage-b/verify-ntfs.ps1 -RequestElevation
```

管理员 runner 在一次会话中顺序建立 1k／10k 新夹具、执行在线验收、等待建库子进程退出、制造真实离线变更、运行新进程恢复。每条原生命令最多 180 秒，管理员子进程最多等待 900 秒；后端每次建库／同步的工作时限 30 秒、最多 100 万条 USN 记录。底层继承阶段 A 的 2 秒 FSCTL 请求期限及取消后完成回收；这不提供驱动异常时绝对硬停止的保证。

每次使用新的证据目录、Cargo target、fixture 与 checkpoint；上次失败的证据不覆盖。已有测试数据不会自动删除。所有 fixture／storage 路径必须位于本工作树 `.scratch/windows-ntfs-stage-b/run/`，在规范化前按原始路径逐级检查并固定非 reparse 祖先。

独立检查器可在完成后运行，不需要管理员、不调用 USN、不写测试目录：

```powershell
python probes/windows-ntfs-stage-b/independent-check.py <fixture> <checkpoint>/inventory.lcusn --count 10000 --verify-native-fixture
# 普通扫描的游标明确为模拟值，必须额外标注：
python probes/windows-ntfs-stage-b/independent-check.py <ordinary-fixture> <checkpoint>/inventory.lcusn --count 1000 --simulated-checkpoint
```

CLI 的 `query <fixture> <checkpoint-directory> <literal>` 重开并同步后，返回状态、完整命中数与前 50 条路径。路径输出为 UTF-16 十六进制码元，保留未配对 surrogate；不会打印工程范围外的名称。

## 后端与持久化契约

`Backend::build/open/sync/query/save/stop` 是本轮私有接口。建库先记录 journal 边界，扫描关系、重放到已观察的安静截止点，再保存完整库存；`open` 从持久游标恢复，不重新 MFT 枚举。调用方显式 `sync`，没有后台线程或自动轮询；Ready 表示最近一次成功同步的截止点，不表示查询时刻的文件系统事务快照。

每批根据对象、父对象和既有硬链接别名收集脏目录。只枚举受影响目录的直接子项，新目录／移入目录递归发现；祖先改名更新 parent/name 关系并派生后代路径，移出／删除子树按可达性清除。目录变化期间失去路径时，候选库存丢弃并从原边界重试，最多 32 次；5 毫秒起步、最高 250 毫秒退避，每 5 毫秒检查取消和原始总期限。30 秒及记录预算仍跨尝试累计。权限、分享、未知版本及预算错误不变成空结果。

硬链接变化不能仅依赖 USN 返回的一个名称。对 `HARD_LINK_CHANGE`，使用完整 64-bit NTFS reference 经 `OpenFileById` 定位对象，再完整枚举当前链接；核对前后对象、serial、link count 和解析路径身份，只返回范围内 parent。范围外名称仅作为单次对象查询的临时 API 结果，不日志记录、不形成持久名称集合。原生查询最多 4096 个链接。解码器保留 V3 的 128-bit identity，而本轮实际 namespace 身份接口为完整 legacy 64-bit reference；任何对象或 parent 高 64 位非零的原生批次明确 Unsupported，不截断、不作为无关事件忽略。依据：[OpenFileById](https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-openfilebyid)、[FILE_ID_DESCRIPTOR](https://learn.microsoft.com/en-us/windows/win32/api/winbase/ns-winbase-file_id_descriptor)、[FindFirstFileNameW](https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-findfirstfilenamew)。

对象身份与目录项关系分开。私有 EntryId 是 `(parent, object, raw name)`，完整路径不是主键；rename 改变关系键，共享稳定 EntryId 生命周期仍待 ADR 确认。来源绑定包括卷 GUID、legacy 32-bit serial、根对象和原始 scope；原生 reference 保留完整值，而非仅保留 MFT slot。强制身份复用尚未原生验证。

v2 容器 magic `LCUSNB2\0`，库存和 volume／journal identity／cursor 同次原子保存。预算：32,768 条、8 MiB 编码、64 层、名称 255 UTF-16 单元、scope 32,768 单元；派生路径总 UTF-16 载荷 32 MiB。预算独立于旧共享引擎 4,096 条限制，并不代表百万级能力。图结构拒绝重复 parent/name、冲突对象属性、目录多名称、环、缺失父目录与 reparse 后代。

同步失败保留最近发布的内存库存和 cursor，状态 Pending；历史查询明确携带 Pending，保存被拒绝。停止释放卷与目录固定句柄，历史查询标为 Stopped，进一步同步被拒绝。损坏、未知格式、来源变化、journal ID 变化、游标低于保留范围或高于当前末尾均明确要求重建；本轮不自动转换 v1 格式。

## 环境、普通权限及已完成的独立验证

当前 OS 原生查询为 Microsoft Windows 11 教育版 10.0.26200 x64；PowerShell 7.6.6；rustc 1.99.0 `b940084d7`、LLVM 23.1.1、x86_64-pc-windows-msvc。

D: 为 NTFS，legacy serial `0xf228c46d`，卷 GUID `\\?\Volume{3b1b89c0-c213-495c-b692-f1e2d9987c9d}\`。普通探针实际 token 未提升、`envbox-runtime64.dll` 未加载；这记录的是当前进程的 API 观察。零访问卷句柄可开，但 QUERY／ENUM／READ 返回 OS 1；GENERIC_READ 卷打开返回 OS 5。日志保留每个错误，不把拒绝解释为可靠空库存。

20 项阶段 B 测试通过；阶段 A 24 项回归通过。涵盖格式／长度／校验、图、名称、硬链接关系、128-bit 名称记录解码、版本拒绝、预算、取消与 Pending／Stopped 行为。真实 Windows `save_new` 对已有目标返回 OS 183；锁住目标后的原子替换返回 OS 5，两者保留精确旧库存和 cursor，并没有遗留本次临时文件。

修正后完整 debug 测试为 24 项通过，新增实际目录项竞争、退避取消／总期限、原生 reference 位宽拒绝及普通权限的对象级硬链接定位。对象定位现在用选定工程根目录的零访问权限句柄作为 `OpenFileById` 的 volume hint，省去不必要的额外卷句柄。普通 token 下，范围外源对象首次在范围内添加链接返回父目录 a，再跨目录添加链接返回 a／b；两条范围内路径的 identity／serial 相同，原生链接数量与全名称枚举数量均为 3。这是实际对象元数据查询证据，不代表普通用户具备 USN 权限。

普通扫描成功证据：`.scratch/windows-ntfs-stage-b/run/native-20261002T204446Z-2ebed87d2d5d41d787156884e7ae2d53/`。1,000 个真实数据集文件，加场景／目录条目共 1,045 条；独立 Python 两次完整遍历的 UTF-16 路径集合、全部对象 reference／serial／attributes 相等，D800 名称保留，v2 快照 71,603 字节。这一轮明确使用 journal=0／cursor=0／假 GUID，是 namespace／codec 验证，不能计入 USN 同步。检查器对工程内 junction 路径实际在遍历前拒绝。

保留首次普通扫描失败证据 `native-20261002T204245Z-f3ef2714a5114aa18791e1d342a37ced/`：实际 OS 3，未保存库存；修正 native identity 扫描所使用的 canonical extended root 后，以上完整检查通过。没有将首次失败隐藏或当成通过。

## 原生 1k／10k 验收

首次管理员执行已授权并运行，代码为 `ce89c1f`，证据 `.scratch/windows-ntfs-stage-b/run/native-20261002T205815Z-f8c13b5fa9324ef58285b09067129ae5/`。fmt、release 的 20 项测试、build、capabilities 均 exit=0；1k 并发建库 exit=1，实际 OS 2，进程总耗时 503 毫秒、句柄 56→56、结束 working set 5,939,200 字节、private commit 1,228,800 字节、peak working set 7,024,640 字节。未保存库存，未执行离线变更／恢复，10k 尚未创建。管理员 child PID 27736 已退出。失败证据保留，没有计作完整通过。

该次 token 实际 elevated=true，Aura 模块未加载。GENERIC_READ 卷句柄和 QUERY／READ 成功；journal ID `0x1d9b89b541688b0`，FirstUsn=37706792960、NextUsn=37746866384、LowestValidUsn=0、API advertised major 2..4；READ 从末尾返回 0 条，因此本轮不能据此宣称实际观察了所有记录版本。ENUM 仅执行 EOF sentinel（OS 38），没有整卷 MFT 建库。20 次原生句柄循环 64→64；capability 进程初始化前后 62→63 的差异也如实保留，不把它解释为已定位的泄漏或零差值。

修正前后的真实竞争回归：扫描器已经取得 `read_dir` 项后，让另一线程实际 rename，再打开对象身份。旧四次立即重试均收到原生 OS 2；在相同前四次实际改名后，新策略第五次扫描成功，1,001 个完整原始路径与独立目录集合一致。此为受控调度的真实文件系统竞争，没有注入错误码，也没有调用或模拟 USN；它证明旧策略缺口，但旧管理员日志缺少阶段标签，尚不能确定首次失败就是该阶段。新日志区分扫描、重放和夹具写入方，同时保留原 OS code，不打印范围外名称。

修正版本的完整 1k／10k 管理员复验仍待新的有范围 UAC 授权；尚未填入成功结果前，不将阶段 B 原生闭环标为完成。

## 无权限／非 NTFS 的降级

复用目录扫描 + ReadDirectoryChangesW。旧共享实现及阶段 A 已有普通权限原生夹具证据，但本轮没有实现第二套通用后端或提升旧上限。降级需要首扫目录、重新启动后的离线校正与丢失／overflow 后重扫；通知不提供与 NTFS journal 等价的持久游标及卷级全序。扫描速度取决于目录 I/O，逐 root／目录覆盖和预算不同，权限不可读要显式 Pending／缩小覆盖。非 NTFS 的实际设备尚未测试，不能用注入错误代表真实内核事实。

## 未验证范围与下一阶段预算

真实 journal 回卷／删除重建、FRN slot 强制复用、真实断电、管理员取消未完成 I/O、驱动异常下硬停止、大小写敏感目录、别名大小写、非 NTFS、网络卷、多卷、长时间压力、冷缓存、100k／1M 均未验证。journal 身份／保留边界／游标失效通过注入测试，未操作真实 journal。V2／V3 解析测试与原生实际版本分别报告；未知／V4 不作为可靠空批次。

100k 阶段先设计存储与查询预算，再授权创建独立夹具；现有 32,768 条容器明确拒绝 100k，不直接扩大常量宣称通过。建议 100k 使用 1k 目录×100 文件，预留 2–4 GiB 空间、10–20 分钟创建／清理窗口、256 MiB 用户进程预算，测真实建库、完整 oracle、重开、局部变化和热查询分布，超限退出并保留证据。预算为规划，需按本轮实测校准，不是承诺。

1M 阶段另行授权，优先专用 NTFS 测试卷／目录，10k 目录×100 文件；预留 10–20 GiB、30–90 分钟窗口和 512 MiB 初始用户进程上限。文件创建、实际监听／journal 同步、库存建库、查询字符串基准分别报告；完整集合可外部排序后流式逐项比对，不能只比较数量或校验和。先完善磁盘／分块索引、增量事务和恢复设计，再谈百万原生结果。任何模拟名称或合成字符串都不算真实文件监听证据。

## Git 记录与本地整理

移除 blanket `/.scratch/`，核心规格／issues 以原路径参与版本管理；仅明确忽略 worktrees、阶段 A/B target／run 和归档。`docs/agents/` 的项目规则、`docs/adr/` 与术语表已纳入版本管理。历史两个规格及八个任务复制原文，未篡改旧状态。

主工作树 `.scratch/loci-v0.1/` 的四个源码／target 副本、两个 ZIP、CI 日志及两个临时索引忽略清单共九项，已移入 `.artifact-archive/loci-local-20261003/`，没有删除内容。活动工作树和原生证据保持原位。主工作树用户移除 `/.scratch/` 的 `.gitignore` 修改及核心记录保留，没有替用户提交 main；本地 `.git/info/exclude` 仅排除活动嵌套工作树。
