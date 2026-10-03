# Windows NTFS / USN Stage A API notes

> 状态：研究笔记，供 `windows-ntfs-stage-a` 独立探针使用。本文记录微软公开文档和本机 SDK 头文件中可以据此实现的约束；“文档支持”不等于本机探针已经成功。实际探针结果、Win32 错误码、耗时和资源数必须以探针输出为准。
>
> 范围：验证 NTFS 的 `FSCTL_ENUM_USN_DATA`、`FSCTL_QUERY_USN_JOURNAL`、`FSCTL_READ_USN_JOURNAL` 路线，以及恢复、对象/目录项和降级边界。本文不改根 Cargo 工程、共享事件/存储/引擎接口，不创建、删除或调整卷 USN Journal，也不把 Linux 游标模型强行解释为 USN。

## 1. 证据边界和当前机器

### 1.1 证据分级

本笔记使用三种标签：

* **文档事实**：有 Microsoft Learn 或 Windows SDK 头文件依据，可以作为 FFI 和状态机约束。
* **待本机实测**：必须由独立探针在用户指定的测试根或明确指定卷上取得，包含成功或失败的实际 Win32 error code。失败是能力结果，不得折叠为空目录。
* **故障注入/设计建议**：例如伪造旧 journal ID 或失效 cursor，用来验证恢复分支；它不能冒充 NTFS 内核真实回卷或重建事件。

### 1.2 本机已知条件

本任务上下文给出的环境是 Windows 11 Education，build 26200；进程为非管理员；`D:` 为 NTFS。研究子任务没有提升权限，没有访问个人目录，没有打开卷设备做 FSCTL 调用，也没有改变系统设置或 USN Journal，因此本文件没有把这些信息当成“本机 API 成功”。探针应在运行时记录：

* `GetVolumeInformationW` 的文件系统名、卷 serial、最大组件长度和 flags；
* 卷 GUID/最终路径，作为来源身份的一部分；
* 当前 token 的管理员/提升状态；
* `FSCTL_QUERY_USN_JOURNAL` 的 journal ID、`FirstUsn`、`NextUsn`、`LowestValidUsn`、`MaxUsn`、容量和记录版本范围；
* 每个失败调用的 Win32 error code；拒绝、不支持、Journal 不活动和 Journal 回卷都要保留为不同结果。

只允许在工程内明确的临时 fixture 根产生文件。默认不枚举整台机器、不扫描用户个人目录、不把文件名写入上传或外部报告；报告可给出计数、时间、错误和经脱敏的相对测试路径。

## 2. 三个核心 FSCTL 的路线

| 控制码 | 输入/输出 | 文档给出的职责 | Stage A 实现约束 |
| --- | --- | --- | --- |
| [`FSCTL_QUERY_USN_JOURNAL`](https://learn.microsoft.com/en-us/windows/win32/api/winioctl/ni-winioctl-fsctl_query_usn_journal) | 无输入；输出 `USN_JOURNAL_DATA_V0/V1/V2` | 查询当前卷 Journal 的 identity、USN 边界和容量 | 在建库前、每个恢复前和发布“完整”前重新查询；不成功就返回 typed error |
| [`FSCTL_ENUM_USN_DATA`](https://learn.microsoft.com/en-us/windows/win32/api/winioctl/ni-winioctl-fsctl_enum_usn_data) | 输入 `MFT_ENUM_DATA_V0/V1`；输出一串变长 USN record | 按 MFT/USN 范围列举已有对象记录 | 只对 NTFS/允许的测试卷调用；第一次 `StartFileReferenceNumber=0`，后续从输出的下一个 FRN 继续 |
| [`FSCTL_READ_USN_JOURNAL`](https://learn.microsoft.com/en-us/windows/win32/api/winioctl/ni-winioctl-fsctl_read_usn_journal) | 输入 `READ_USN_JOURNAL_DATA_V0/V1`；输出一串变长 USN record | 按 USN 选择增量变更，可等待新记录 | 用 journal ID + cursor 一起读取；`ERROR_JOURNAL_ENTRY_DELETED`、ID 不匹配、不可读都要求重建或降级 |

Microsoft 的驱动文档明确区分了用途：`ENUM` 用于 listing/enumeration，`READ` 用于按 USN 选择记录；二者都通过 `DeviceIoControl`（或驱动层等价调用）返回变长记录。[FSCTL_ENUM_USN_DATA 驱动说明](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/ntifs/ni-ntifs-fsctl_enum_usn_data)、[FSCTL_READ_USN_JOURNAL 驱动说明](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/ntifs/ni-ntifs-fsctl_read_usn_journal)

### 2.1 卷句柄和只读能力探测

Win32 页面要求把目标卷作为 `\\.\X:` 打开，然后把句柄传给 `DeviceIoControl`；卷必须支持相应的文件系统。`GetVolumeInformationW` 返回卷 serial、最大文件名组件长度、文件系统名和能力 flags，其中包括 `FILE_SUPPORTS_USN_JOURNAL`、`FILE_SUPPORTS_REPARSE_POINTS`、`FILE_CASE_SENSITIVE_SEARCH`、`FILE_CASE_PRESERVED_NAMES` 和 `FILE_UNICODE_ON_DISK`。[GetVolumeInformationW](https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-getvolumeinformationw)、[Obtaining Volume Information](https://learn.microsoft.com/en-us/windows/win32/fileio/obtaining-volume-information)

`FILE_ID_INFO` 提供卷 serial 和 128-bit `FileId`；`GetFinalPathNameByHandleW` 可以取得 `VOLUME_NAME_GUID` 形式的 `\\?\Volume{...}\` 路径。建议把卷 GUID（若能取得）、serial 和探针实际打开的 volume path 一起作为来源身份，不能只把 `D:` 盘符当恒定身份。[FILE_ID_INFO](https://learn.microsoft.com/en-us/windows/win32/api/winbase/ns-winbase-file_id_info)、[GetFinalPathNameByHandleW](https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-getfinalpathnamebyhandlew)

公开的 change-journal identifier 文档写明：查询和其他 change-journal 操作需要 system administrator privileges；在当前非管理员会话中必须把拒绝作为待测的预期分支，并记录实际 code，而不是偷偷提升。[Using the Change Journal Identifier](https://learn.microsoft.com/en-us/windows/win32/fileio/using-the-change-journal-identifier)

### 2.2 `MFT_ENUM_DATA_V0/V1`

[`MFT_ENUM_DATA_V0`](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/ntifs/ns-ntifs-mft_enum_data_v0) 的布局是：

```text
ULONGLONG StartFileReferenceNumber;
USN       LowUsn;
USN       HighUsn;
```

第一次枚举将 `StartFileReferenceNumber` 设为 0。返回缓冲区前缀/首项给出下一次要使用的 `StartFileReferenceNumber`；后续调用必须采用该值推进，不能以“最后一条 record 的 USN + 1”代替，因为 USN 是用于排序/选择的卷内序列值而不是对象身份。`LowUsn` 和 `HighUsn` 是记录最后 USN 的范围；具体边界以 API 文档和探针记录为准。

[`MFT_ENUM_DATA_V1`](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/ntifs/ns-ntifs-mft_enum_data_v1) 在这三个字段后增加：

```text
USHORT MinMajorVersion;
USHORT MaxMajorVersion;
```

文档说明 V1 用于支持含 128-bit file identifier 的 ReFS 场景；`MinMajorVersion=2`、`MaxMajorVersion=3` 表示接受 V2/V3。Stage A 的目标是 NTFS，但解析器仍应检查 `MajorVersion`，不可把 V1 结构或未来版本静默按 V0/V2 解码。

### 2.3 `USN_JOURNAL_DATA_V0/V1/V2`

[`USN_JOURNAL_DATA_V0`](https://learn.microsoft.com/en-us/windows/win32/api/winioctl/ns-winioctl-usn_journal_data_v0) 的关键字段是：

```text
DWORDLONG UsnJournalID;
USN       FirstUsn;
USN       NextUsn;
USN       LowestValidUsn;
USN       MaxUsn;
DWORDLONG MaximumSize;
DWORDLONG AllocationDelta;
```

语义：

* `UsnJournalID` 是当前 Journal instance 的 identity；它变化意味着旧 checkpoint 不能继续信任。
* `FirstUsn` 是当前可读的最早 USN；`NextUsn` 是下一个将写入的位置，可作为一次读边界的候选快照。
* `LowestValidUsn` 用于识别当前实例的有效低端；旧的 USN 可能已因容量回收而不可读。
* `MaxUsn` 是 USN 上限；`MaximumSize` 和 `AllocationDelta` 是 Journal 容量信息，不是应用层索引大小。

V1 增加 `MinSupportedMajorVersion` / `MaxSupportedMajorVersion`；[USN_JOURNAL_DATA_V1](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/ntifs/ns-ntifs-usn_journal_data_v1)。V2 再增加 range-tracking 信息；[USN_JOURNAL_DATA_V2](https://learn.microsoft.com/en-us/windows/win32/api/winioctl/ns-winioctl-usn_journal_data_v2)。没有实测确认 range tracking 时，不能用 V4 语义或它的容量字段推导普通 NTFS 的路径完整性。

### 2.4 `READ_USN_JOURNAL_DATA_V0/V1`

[`READ_USN_JOURNAL_DATA_V0`](https://learn.microsoft.com/en-us/windows/win32/api/winioctl/ns-winioctl-read_usn_journal_data_v0) 布局：

```text
USN       StartUsn;
DWORD     ReasonMask;
DWORD     ReturnOnlyOnClose;
DWORDLONG Timeout;
DWORDLONG BytesToWaitFor;
DWORDLONG UsnJournalID;
```

`StartUsn=0` 从当前最早记录开始；如果非零 start 已低于 Journal 的最早有效记录，文档指定 `ERROR_JOURNAL_ENTRY_DELETED`。读取必须把当前 `UsnJournalID` 原样带回；Journal 停止再启动、删除再创建或 NTFS 判断旧记录可能不可用时会换 ID，ID 不匹配时调用失败。不要只保存 cursor 而遗漏 ID。[READ_USN_JOURNAL_DATA_V0](https://learn.microsoft.com/en-us/windows/win32/api/winioctl/ns-winioctl-read_usn_journal_data_v0)、[Using the Change Journal Identifier](https://learn.microsoft.com/en-us/windows/win32/fileio/using-the-change-journal-identifier)

`ReasonMask` 是过滤条件；完整 correctness 验收应先使用覆盖需要的 mask（含 create/delete/rename/hard-link/reparse/stream 和 close 等需要观察的 reason），而不是为了少读记录而默认丢掉变化。文档说明：USN reason 会在对象打开期间累积，并在 close 时生成最终 close record；重复写入不应被误解为一条一一对应的用户操作。`USN_REASON_NAMED_DATA_*` 和 `USN_REASON_STREAM_CHANGE` 是 named stream/ADS 的线索，需重新读取对象状态，不能把 reason 直接当作独立目录项。[READ_USN_JOURNAL_DATA_V0](https://learn.microsoft.com/en-us/windows/win32/api/winioctl/ns-winioctl-read_usn_journal_data_v0)、[Change Journal Records](https://learn.microsoft.com/en-us/windows/win32/fileio/change-journal-records)

`BytesToWaitFor=0` 会在到达 Journal 尾部时返回，但返回量不可预知，官方提示存在输出缓冲区溢出风险；反复排空时应使用非零阈值和有界 output buffer，只有进入等待新记录状态时才考虑零值。异步句柄会忽略 `Timeout`；非零 `BytesToWaitFor` 在达到尾部时可能使操作一直 pending，直到达到阈值、超时策略满足或取消。[READ_USN_JOURNAL_DATA_V0 的 Timeout/BytesToWaitFor 说明](https://learn.microsoft.com/en-us/windows/win32/api/winioctl/ns-winioctl-read_usn_journal_data_v0)

[`READ_USN_JOURNAL_DATA_V1`](https://learn.microsoft.com/en-us/windows/win32/api/winioctl/ns-winioctl-read_usn_journal_data_v1) 在 V0 后增加 `MinMajorVersion` 和 `MaxMajorVersion`，用于约束接受的 Journal record major version。该结构首先用于含 128-bit file identifier 的 ReFS；NTFS Stage A 仍应按返回 record 的 `MajorVersion` 做运行时校验。

## 3. 输出缓冲区和 record 解析

### 3.1 V2/V3 的共同前缀

V2 是最低兼容记录版本。V2/V3 具有这些逻辑字段：

```text
DWORD         RecordLength;
WORD          MajorVersion;
WORD          MinorVersion;
FILE_ID       FileReferenceNumber;        // V2: 64-bit, V3: FILE_ID_128
FILE_ID       ParentFileReferenceNumber;  // V2: 64-bit, V3: FILE_ID_128
USN           Usn;
LARGE_INTEGER TimeStamp;
DWORD         Reason;
DWORD         SourceInfo;
DWORD         SecurityId;
DWORD         FileAttributes;
WORD          FileNameLength;             // bytes
WORD          FileNameOffset;             // from record start
WCHAR         FileName[1];
```

[`USN_RECORD_V2`](https://learn.microsoft.com/en-us/windows/win32/api/winioctl/ns-winioctl-usn_record_v2) 使用 64-bit file/parent reference；[`USN_RECORD_V3`](https://learn.microsoft.com/en-us/windows/win32/api/winioctl/ns-winioctl-usn_record_v3) 使用 `FILE_ID_128`，并且官方页面列出 V2/V3/V4 的 `MajorVersion` 选择规则。

必须遵守：

1. 用 `RecordLength` 跳到下一条记录；不要用 `sizeof(USN_RECORD)` 或固定长度。
2. 用 `FileNameOffset` 在运行时定位名称；不要根据 C 结构最后一个 `WCHAR[1]` 做编译期指针运算。
3. `FileNameLength` 是字节数，不保证结尾存在 `\\0`；保留精确的原始 UTF-16 code units，再按长度转换。
4. output 中 records 按 64-bit 边界对齐；解析前检查 `RecordLength`、offset、name length 和缓冲区剩余长度，任一越界都返回损坏/不支持错误。
5. `MajorVersion=2/3/4` 决定布局；已知 major 的未知 minor 应用 `RecordLength` 兼容，未知 major 必须停止发布完整状态并报告不支持。

从 Microsoft 页面列出的最低客户端版本可得到这条实现参考线：V2/旧 `USN_RECORD` 从 Windows XP 支持；V3 从 Windows 8 支持；V4 从 Windows 8.1 支持。它们是 SDK/OS 的最低文档版本，不是当前卷一定会输出的版本范围；探针仍须在 `QUERY`/实际 record 上记录可接受的 major 范围，并对目标 Windows build 26200 的真实输出做验证。[USN_RECORD_V2](https://learn.microsoft.com/en-us/windows/win32/api/winioctl/ns-winioctl-usn_record_v2)、[USN_RECORD_V3](https://learn.microsoft.com/en-us/windows/win32/api/winioctl/ns-winioctl-usn_record_v3)、[USN_RECORD_V4](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/ntifs/ns-ntifs-usn_record_v4)

官方还说明 `FileReferenceNumber` 是关联记录与文件的任意分配值，`ParentFileReferenceNumber` 是关联记录与父目录的任意分配值；USN record 不是完整路径，必须由 parent/name 关系重建目录项。[USN_RECORD_V3 字段和版本说明](https://learn.microsoft.com/en-us/windows/win32/api/winioctl/ns-winioctl-usn_record_v3)、[MS-FSCC USN record protocol](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-fscc/d2a2b53e-bf78-4ef3-90c7-21b918fab304)

### 3.2 V4 不含文件名

[`USN_RECORD_V4`](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/ntifs/ns-ntifs-usn_record_v4) 的布局为：

```text
USN_RECORD_COMMON_HEADER Header;
FILE_ID_128              FileReferenceNumber;
FILE_ID_128              ParentFileReferenceNumber;
USN                      Usn;
ULONG                    Reason;
ULONG                    SourceInfo;
ULONG                    RemainingExtents;
USHORT                   NumberOfExtents;
USHORT                   ExtentSize;
USN_RECORD_EXTENT        Extents[1];
```

V4 是 range tracking 记录，不含 V2/V3 的 `FileName`。在启用 range tracking 的条件下，NTFS 可能只输出 V3；V4 记录与 filename-bearing V3 的关系必须按文档和实测确认。Stage A 解析器遇到 V4 时不能把 extent bytes 当作 UTF-16 名称；若当前 probe 不实现 V4，应报告“记录版本不支持/需重建或降级”，不能丢记录后继续声称完整。[USN_RECORD_V4](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/ntifs/ns-ntifs-usn_record_v4)、[USN_RECORD_V3 的 range-tracking 备注](https://learn.microsoft.com/en-us/windows/win32/api/winioctl/ns-winioctl-usn_record_v3)

### 3.3 缓冲区首值、推进和范围

微软的 [Walking a Buffer of Change Journal Records](https://learn.microsoft.com/en-us/windows/win32/fileio/walking-a-buffer-of-change-journal-records) 说明输出是前缀值加若干变长 records，必须按 `RecordLength` 遍历；但 `ENUM` 的专用文档定义前缀为下一个 `StartFileReferenceNumber`，而 `READ` 的推进值来自读取结果中的下一个 USN。实现应以各控制码的结构/文档为准，不把两个前缀混成一个类型。

USN 只能在同一卷、同一 Journal instance 的 cursor 语义中使用。不要把 `last_usn + 1` 当作通用推进算法，也不要在 journal ID 改变后继续比较旧 USN。应用层应保存“下一次读取的 cursor”，并在每次读取后采用内核返回的 next cursor。

## 4. 建库一致性：建议的两阶段算法

目标是证明“枚举得到的基线 + 读取到截止点之前的增量”与一个独立的完整路径集合相同。`count`、checksum 或前 50 项不足以证明路径集合一致。

### 4.1 建议的顺序

1. 只打开选定的测试卷和工程内 fixture 根；执行 volume capability/权限探测。非 NTFS、无权限、Journal 未激活或 API 不可用时立刻返回明确错误或降级建议。
2. `QUERY` 记录 `J0 = {volume_identity, journal_id, FirstUsn, LowestValidUsn, NextUsn, MaxUsn, version_range}`。令 `S = J0.NextUsn` 作为初始读取边界候选；同时保存查询时间和实际 Win32 结果。
3. 用 `MFT_ENUM_DATA_V0`（必要时 V1）做有界批次枚举。每次按 output 的下一个 `StartFileReferenceNumber` 推进；`LowUsn/HighUsn` 不超出本次计划的范围。解析 V2/V3；遇到未知 major、损坏 record 或实际错误立即失败，不能发布“完整”索引。
4. 枚举期间由 fixture writer 只执行预先定义的 add/delete/file rename/directory rename/hard-link/ADS/reparse 操作。writer 不接触用户目录。
5. writer 停止后再次 `QUERY` 得到 `J1`，令 `C = J1.NextUsn`。如果 `journal_id` 变了、旧 `S` 已低于当前 `FirstUsn/LowestValidUsn`，或查询不成功，建库失败并要求 rebuild。
6. 从 `S` 读取到 `C` 之前的记录，使用 journal ID，采用输出的下一个 cursor 逐批 drain。实现应定义并记录边界（例如只应用 `S <= usn < C` 的记录，最终再以 query 确认到达 C）；不要假设 USN 连续。
7. 以独立的 `std::fs`/Win32 目录遍历获取 fixture 在相同稳定截止点的**完整相对路径集合**。对每一项保留原始名称、目录/文件类型和约定的 reparse/ADS 处理；排序后逐项比较，失败时打印经脱敏的差异摘要，不只比较 count/checksum/前 50 项。
8. 只有在 Journal identity、cursor、版本解析、枚举、增量重放和完整路径集合全部通过时，才写入“validated/stable”状态。

`S=NextUsn` 只是定义本次实验边界的方案，必须以探针实际返回的 Journal 字段和观察到的 fixture 操作时序验证；USN 在查询后仍可有并发写入，所以如果 writer 未在第二次 query 前停止，不能声称 C 是稳定截止点。若变化在两次 query 之间持续，重复捕获边界或明确报告“无法建立稳定截止点”。

### 4.2 目录项和硬链接

一个 USN record 只携带一个 `FileName`/parent 关系，不能假设一次 MFT/USN 记录列举就给出对象的所有硬链接路径。官方的 [`FindFirstFileNameW`](https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-findfirstfilenamew) / [`FindNextFileNameW`](https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-findnextfilenamew) 用于枚举一个文件的所有 hard-link names；结束时 `FindNextFileNameW` 返回 `ERROR_HANDLE_EOF`，句柄必须 `FindClose`。`StringLength` 是字符数，遇到 `ERROR_MORE_DATA` 时按 API 提示扩充缓冲区；Windows 10 1607+ 的长路径行为仍需按实际 manifest/策略验证。

因此共享架构建议：

```text
Source/VolumeIdentity
  ObjectId = (volume identity, 64-bit or 128-bit file reference)
  EntryId  = (ObjectId, parent ObjectId, raw UTF-16 name, link/entry discriminator)
```

同一 object 可以有多个 `EntryId`；保存 parent/name 关系和平台原始名称，完整路径只做派生视图，不做唯一主键。`USN_REASON_HARD_LINK_CHANGE` 只能提示重新枚举该对象的全部链接，不能直接当作“新增一个已知路径”。[Hard Links and Junctions](https://learn.microsoft.com/en-us/windows/win32/fileio/hard-links-and-junctions)、[fsutil hardlink](https://learn.microsoft.com/en-us/windows-server/administration/windows-commands/fsutil-hardlink)

## 5. 名称、ADS、reparse 和大小写边界

| 能力 | 文档支持/实现要求 | Stage A 验证状态 |
| --- | --- | --- |
| 原始 UTF-16 | USN `FileName` 是 Unicode、长度由 `FileNameLength`（字节）给出，不保证 NUL；先保留 UTF-16 code units。 | 结构规则已由文档确认；实际非 BMP、组合字符、保留/不成对 surrogate 的 fixture 行为待测 |
| 长路径 | [`Naming Files, Paths, and Namespaces`](https://learn.microsoft.com/en-us/windows/win32/fileio/naming-a-file) 说明 `\\?\` 语法和 Unicode API；`FindFirstFileNameW` 也有长路径说明。 | 待测；必须记录是否依赖 long-path policy/manifest，不能仅凭 API 名称宣称无限长度 |
| ADS / named stream | `USN_REASON_NAMED_DATA_*`、`USN_REASON_STREAM_CHANGE` 表明 stream 变化；[File Streams](https://learn.microsoft.com/en-us/windows/win32/fileio/file-streams) 说明 `file:stream:$DATA` 语法。USN `FileAttributes` 不含 stream attributes。 | 创建 ADS 并核对 USN reason/最终对象状态待测；ADS 名称不应无条件变成普通目录项 |
| reparse/junction/symlink | [`Reparse Points`](https://learn.microsoft.com/en-us/windows/win32/fileio/reparse-points) 和 [`Symbolic Link Effects`](https://learn.microsoft.com/en-us/windows/win32/fileio/symbolic-link-effects-on-file-systems-functions) 说明 `FILE_FLAG_OPEN_REPARSE_POINT` 可打开 link 本身；普通打开可能跟随 target。 | 待测；默认不跟随 fixture 外的 reparse，记录 reparse 属性并避免目录逃逸/重复遍历 |
| 大小写保留/敏感 | `GetVolumeInformationW` 的 `FILE_CASE_SENSITIVE_SEARCH` 是卷能力；`FILE_CASE_SENSITIVE_INFORMATION` 的 `FILE_CS_FLAG_CASE_SENSITIVE_DIR` 是目录级状态。 | 待测；要在大小写敏感目录中同时建立 `A`/`a`（若权限/策略允许），比较路径集合和查询语义，不能从卷 flag 推断每个目录都敏感 |

NTFS 会保留 Unicode 名称和原始大小写，但名称比较语义仍需按卷/目录和 Win32 访问路径实测；[Naming Files, Paths, and Namespaces](https://learn.microsoft.com/en-us/windows/win32/fileio/naming-a-file) 明确提示不要假设所有目录都大小写敏感。

## 6. 持久 checkpoint 和恢复

最小的 NTFS 专属 checkpoint 建议为：

```text
source_kind       = ntfs_usn
volume_identity   = volume GUID/serial + 需要时的 root/source discriminator
journal_id        = USN_JOURNAL_DATA.*.UsnJournalID
cursor_usn        = 下一次 FSCTL_READ_USN_JOURNAL 的 StartUsn
accepted_versions = [min_major, max_major]
last_boundary     = 最近一次完成验证的 cutoff（可选，但应记录）
```

重开时必须先 `QUERY`，再按以下决策：

| 观察到的情况 | 处理 | 可否当空结果 |
| --- | --- | --- |
| volume identity、journal ID 相同，cursor 不低于可读下界，版本可解析 | 从 cursor 读取到新 cutoff，完成后更新 checkpoint | 否，正常增量 |
| journal ID 变化 | 丢弃旧增量连续性，做完整 rebuild 或明确降级 | 否 |
| cursor < `FirstUsn`/`LowestValidUsn`，或 `ERROR_JOURNAL_ENTRY_DELETED` | Journal 回卷/旧记录被删除；做 rebuild 或降级 | 否 |
| `ERROR_JOURNAL_NOT_ACTIVE`、`ERROR_JOURNAL_DELETE_IN_PROGRESS` | 标记 source unavailable；等待后重新探测或 fallback | 否 |
| 非 NTFS、权限拒绝、unsupported major、解析错误或卷不可读 | 返回 typed error；不能伪造空索引 | 否 |

`Using the Change Journal Identifier` 说明：Journal 可能被停止/重启、删除/重建，或因旧 record 可能不可用而被 restamp；前一种情况下 USN 可能从零重新开始，后一种可以继续当前 USN 但 ID 变化。两种都要求 ID + USN 联合校验。[Change Journal identifier](https://learn.microsoft.com/en-us/windows/win32/fileio/using-the-change-journal-identifier)

应把这些系统错误保留为结构化结果：`ERROR_JOURNAL_DELETE_IN_PROGRESS`（1178）、`ERROR_JOURNAL_NOT_ACTIVE`（1179）、`ERROR_JOURNAL_ENTRY_DELETED`（1181）；[system error codes 1000–1299](https://learn.microsoft.com/en-us/windows/win32/debug/system-error-codes--1000-1299-)。故障注入可以伪造 journal ID、旧 cursor、unsupported major 和中途 I/O error，验证“要求 rebuild/降级且不发布完整状态”；报告中必须标注为模拟，不得称为真实内核回卷。

## 7. OVERLAPPED、取消和资源生命周期

`DeviceIoControl` 使用 `FILE_FLAG_OVERLAPPED` 打开的句柄时，必须传有效的 `OVERLAPPED`，并在其中准备 event；pending 时返回 `FALSE`/`ERROR_IO_PENDING`，完成后通过 `GetOverlappedResult` 取得实际字节数。`lpBytesReturned` 在异步 pending 期间不能当作有效记录长度。[DeviceIoControl](https://learn.microsoft.com/en-us/windows/win32/api/ioapiset/nf-ioapiset-deviceiocontrol)、[GetOverlappedResult](https://learn.microsoft.com/en-us/windows/win32/api/ioapiset/nf-ioapiset-getoverlappedresult)

停止/取消时：

1. 保持 output/input buffer、`OVERLAPPED` 和 event 存活；调用 `CancelIoEx(volume_handle, &overlapped)`。
2. `ERROR_NOT_FOUND` 只表示没有找到尚未完成的请求；不能因此提前释放资源。
3. `CancelIoEx` 不等待，必须用 `GetOverlappedResult(..., TRUE)` 等到最终完成；完成结果可能是 `ERROR_OPERATION_ABORTED`，也可能正常完成或报告其他实际错误。
4. 只有确认完成后才释放 buffer/event/handle。把 `ERROR_OPERATION_ABORTED` 归类为用户请求的 clean stop，其他 error 保留为失败；[CancelIoEx](https://learn.microsoft.com/en-us/windows/win32/api/ioapiset/nf-ioapiset-cancelioex)、[Canceling Pending I/O Operations](https://learn.microsoft.com/en-us/windows/win32/fileio/canceling-pending-i-o-operations)

实现应有有界 batch/output buffer、最大待处理 read、停止 deadline 和 cancellation path；解析不应把整卷 Journal/MFT 一次性放入内存。报告至少记录 open/query/enum/read/replay/reopen 的 wall time、进程用户空间内存（如 private working set/heap，定义要固定）、句柄数和 buffer 峰值；不要把这些数冒充 NTFS 内核 Journal 内存。

## 8. 普通用户、非 NTFS 和 unprivileged 候选路径

### 8.1 普通用户与管理员

标准 change-journal 文档将 Query 和其他 change-journal 操作标为需要管理员。Stage A 需在当前非管理员进程实际调用并报告：volume open 是否成功、Query/Enum/Read 各自的 Win32 code、管理员重跑（若用户另外提供授权环境）的差异。任务禁止静默提权。

创建/修改/删除 Journal 是另一组管理操作；[Creating, Modifying, and Deleting a Change Journal](https://learn.microsoft.com/en-us/windows/win32/fileio/creating-modifying-and-deleting-a-change-journal) 说明这些操作需要管理员，删除期间读取/创建/修改会失败。探针不得调用这些 FSCTL，不得改变卷 Journal。

### 8.2 `FSCTL_READ_UNPRIVILEGED_USN_JOURNAL`

本机 Windows SDK `C:\Program Files (x86)\Windows Kits\10\Include\10.0.26100.0\um\winioctl.h` 声明了：

```c
#define FSCTL_READ_UNPRIVILEGED_USN_JOURNAL \
    CTL_CODE(FILE_DEVICE_FILE_SYSTEM, 234, METHOD_NEITHER, FILE_ANY_ACCESS)
```

即 `0x903AB`（以本机头文件宏展开为准）。本次 Microsoft Learn 公开文档检索没有找到该控制码的公开语义契约；因此它只能作为当前系统上的可选探针候选，不能替代有文档支持的 `QUERY/ENUM/READ` correctness 基线。若探针测试它，必须单独记录：成功/失败 code、返回 record major、是否包含 filename/parent、是否能完成完整路径集合 oracle；即使成功也只能报告“部分可用”，除非独立完整集合验证通过。

### 8.3 降级建议

没有 NTFS/权限/Journal 连续性时，复用现有的目录遍历 + `ReadDirectoryChangesW` 路线：

* 目录遍历可以在某个稳定时点得到精确的路径集合，但成本随路径数增长，范围是指定 root，不是卷级 MFT；扫描期间仍需重新确认并发变更。
* `ReadDirectoryChangesW` 是 root-scoped 通知，缓冲区溢出或 watcher 停止会丢事件，必须重新扫描；它不会提供 Journal 的跨进程/跨重启 cursor 语义。
* USN Journal 是卷级、持久且适合快速枚举/增量的候选，但需要 NTFS/权限，并且按卷返回，必须在应用层过滤测试 root，不能把它当作只返回某个目录的安全边界。

任何 fallback 都应返回能力和语义差异；权限拒绝、非 NTFS、Journal 回卷和真实 I/O 错误不能包装成“空搜索结果”。

## 9. 10 万/100 万真实条目的验证预算

本阶段只在小型真实 NTFS fixture 做完整 correctness；不自动创建百万文件、不扫整盘，也不把百万个合成字符串当作百万文件监听结果。后续需要明确批准和专用测试路径后，按下面预算逐档执行：

| 档位 | 真实数据 | 必做证据 | 资源预算/记录 |
| --- | --- | --- | --- |
| 校准 | 1k–10k 条文件/目录，含 rename/delete/hard-link/ADS/reparse/Unicode 样例 | 枚举 + 增量 replay + 独立完整路径集合逐项相等 | cold/warm wall time、CPU、用户空间内存峰值、句柄、批次/记录数 |
| 10 万 | 至少 100,000 个真实 NTFS directory entries（文件和目录数分别记录） | 并发 writer、稳定 cutoff、完整递归路径集合 oracle；失败不得发布 validated | 固定 fixture 生成时间和硬件；报告 p50/p95、最大 buffer、Journal lag、重放速率、停止时间 |
| 100 万 | 至少 1,000,000 个真实 NTFS entries，使用专用测试根/卷 | 仍需完整集合校验；允许按确定性 shards 生成和比较，但最终集合必须覆盖全部条目，不得只验 count/checksum/前 50 | 先估算磁盘空间、创建时长、Journal 容量和回卷风险；控制并发，必要时分阶段，任何超预算自动停止 |

每档都应固定 fixture manifest（相对路径、类型、预期 hard-link 数、reparse/ADS 标记），并由独立目录遍历生成 oracle；不把探针自己写出的索引作为 oracle。预算数字是计划门槛而不是已经测得的性能承诺，真正阈值应在校准档由机器实测后确定。建议分别测 cold cache/warm cache、枚举阶段和 replay 阶段，区分用户空间内存与内核 Journal 开销。

## 10. 给共享核心的接口需求（只报告，不在本阶段修改）

共享层至少需要以下概念：

* `SourceIdentity` / `VolumeIdentity`：卷 GUID/serial/平台来源类型；驱动和盘符变化不能悄悄变成同一来源。
* `ObjectId`：NTFS 64/128-bit file reference 与来源身份绑定；要考虑对象身份复用，不能跨 Journal instance 无限期相信旧 FRN。
* `EntryId`：`ObjectId + parent ObjectId + raw name + entry/link discriminator`；同一 hard-link object 的多个搜索路径必须保留。
* `RawName`：UTF-16 code units/平台原始名称和长度；规范化、大小写折叠和完整路径应是派生策略。
* `SourceCursor`：NTFS 专属 `{journal_id, cursor_usn, accepted_record_versions}`，不要要求 Linux source 也伪造 USN 字段。
* `Capability`：filesystem、管理员/权限、Journal 是否 active、record versions、reparse/case/long-path 能力及降级原因。
* `SourceError`：`PermissionDenied`、`NotNtfs`、`JournalNotActive`、`JournalEntryDeleted/Gap`、`JournalDeleteInProgress`、`UnsupportedRecordVersion`、`Io` 等可区分错误；这些都不能转为空索引。
* 状态发布：只有在 source identity、journal identity、稳定 cutoff、完整枚举/replay、record parser 和独立路径 oracle 通过后才进入 `validated/complete`；否则是 `partial`/`needs_rebuild`/`unavailable`。

建议抽象成“能力探测 → 初始边界 → 有界枚举批次 → 增量 replay → checkpoint”的来源适配器；不把完整路径当主键，也不把平台 cursor 混成共享的无类型整数。

## 11. 参考链接

* [FSCTL_ENUM_USN_DATA](https://learn.microsoft.com/en-us/windows/win32/api/winioctl/ni-winioctl-fsctl_enum_usn_data)
* [FSCTL_QUERY_USN_JOURNAL](https://learn.microsoft.com/en-us/windows/win32/api/winioctl/ni-winioctl-fsctl_query_usn_journal)
* [FSCTL_READ_USN_JOURNAL](https://learn.microsoft.com/en-us/windows/win32/api/winioctl/ni-winioctl-fsctl_read_usn_journal)
* [MFT_ENUM_DATA_V0](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/ntifs/ns-ntifs-mft_enum_data_v0) / [MFT_ENUM_DATA_V1](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/ntifs/ns-ntifs-mft_enum_data_v1)
* [READ_USN_JOURNAL_DATA_V0](https://learn.microsoft.com/en-us/windows/win32/api/winioctl/ns-winioctl-read_usn_journal_data_v0) / [V1](https://learn.microsoft.com/en-us/windows/win32/api/winioctl/ns-winioctl-read_usn_journal_data_v1)
* [USN_JOURNAL_DATA_V0](https://learn.microsoft.com/en-us/windows/win32/api/winioctl/ns-winioctl-usn_journal_data_v0) / [V1](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/ntifs/ns-ntifs-usn_journal_data_v1) / [V2](https://learn.microsoft.com/en-us/windows/win32/api/winioctl/ns-winioctl-usn_journal_data_v2)
* [USN_RECORD_V2](https://learn.microsoft.com/en-us/windows/win32/api/winioctl/ns-winioctl-usn_record_v2) / [V3](https://learn.microsoft.com/en-us/windows/win32/api/winioctl/ns-winioctl-usn_record_v3) / [V4](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/ntifs/ns-ntifs-usn_record_v4)
* [Walking a Buffer of Change Journal Records](https://learn.microsoft.com/en-us/windows/win32/fileio/walking-a-buffer-of-change-journal-records)
* [Using the Change Journal Identifier](https://learn.microsoft.com/en-us/windows/win32/fileio/using-the-change-journal-identifier)
* [Creating, Modifying, and Deleting a Change Journal](https://learn.microsoft.com/en-us/windows/win32/fileio/creating-modifying-and-deleting-a-change-journal)
* [Change Journal Records](https://learn.microsoft.com/en-us/windows/win32/fileio/change-journal-records)
* [DeviceIoControl](https://learn.microsoft.com/en-us/windows/win32/api/ioapiset/nf-ioapiset-deviceiocontrol) / [GetOverlappedResult](https://learn.microsoft.com/en-us/windows/win32/api/ioapiset/nf-ioapiset-getoverlappedresult) / [CancelIoEx](https://learn.microsoft.com/en-us/windows/win32/api/ioapiset/nf-ioapiset-cancelioex)
* [FindFirstFileNameW](https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-findfirstfilenamew) / [FindNextFileNameW](https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-findnextfilenamew)
* [GetVolumeInformationW](https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-getvolumeinformationw) / [FILE_ID_INFO](https://learn.microsoft.com/en-us/windows/win32/api/winbase/ns-winbase-file_id_info)
* [Naming Files, Paths, and Namespaces](https://learn.microsoft.com/en-us/windows/win32/fileio/naming-a-file) / [File Streams](https://learn.microsoft.com/en-us/windows/win32/fileio/file-streams) / [Reparse Points](https://learn.microsoft.com/en-us/windows/win32/fileio/reparse-points)
* [FILE_CASE_SENSITIVE_INFORMATION](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/ntifs/ns-ntifs-_file_case_sensitive_information)
