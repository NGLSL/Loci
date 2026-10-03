# Windows NTFS 原型向共享核心的接口交接

状态：等待双方对齐，接口建议尚未冻结。日期：2026-10-03。

本轮只整理交接材料。Windows 阶段 B 的独立实现与 1k／10k 真实夹具已经完成；Linux 的新产品后端与共享装配仍由另一会话负责。本文件不宣称对方已实现或接受以下建议。

## 事实基线与阅读入口

- 本机 main：`e204917644fb771e5c92e74d130a34127ca8f74e`；未修改或合并。
- 实际原生验收源码：`e9684d5988b1b963c2091426e998b7f35eeb18b8`。
- 阶段 B 完整交付：`8607bb5584b642dafb9e3edc766b78a63b959f66`，分支 `codex/windows-ntfs-stage-b`。本次后续提交仅整理文档。
- [阶段 B 实测报告](../probes/windows-ntfs-stage-b/REPORT.md)：真实 V2、权限、硬链接、完整原始路径集合及未验证项。
- [ADR-0001](adr/0001-source-object-entry-cursor.md)：身份与游标的 proposed 提案。
- [现有 v0.1 接口](V0.1-INTERFACES.md)与[持久化契约](PERSISTENCE.md)：旧有界引擎的已冻结接口，当前没有修改。

Windows 私有入口在 [backend.rs](../probes/windows-ntfs-stage-b/src/backend.rs)，库存容器在 [store.rs](../probes/windows-ntfs-stage-b/src/store.rs)，硬链接校正在 [native.rs](../probes/windows-ntfs-stage-b/src/native.rs)；原始记录和关系键在阶段 A 的 [model.rs](../probes/windows-ntfs-stage-a/src/model.rs)，游标校验在 [checkpoint.rs](../probes/windows-ntfs-stage-a/src/checkpoint.rs)。这些私有类型不是待直接复制到公共接口的 ABI。

## 身份、范围与名称

| 概念 | Windows 当前事实 | 共享建议／待确认点 |
| --- | --- | --- |
| 来源身份 | 卷 GUID 与 legacy 32-bit serial；序列化字段为 u64，不代表测得完整 64-bit serial | 来源标识需带 provider／身份方案，不能仅比较一个数值或盘符；Linux 具体来源身份由其会话提供 |
| 搜索范围 | 额外绑定根对象与 canonical 原始 UTF-16 scope；目录固定句柄防止重定向 | 区分来源和范围：同卷两个 root 可以是不同 Scope，但其对象身份不因此变成两个物理对象；重叠范围的展示去重策略待定 |
| 对象身份 | 完整 legacy 64-bit NTFS reference 存于 u128；保留完整返回值，不只保存 MFT slot | 对象键应由来源与 provider 原始身份组成，不能把 Linux inode 数值与 NTFS reference 混用；删除后复用如何区分需平台证据 |
| 目录项关系 | `EntryId { parent: u128, object: u128, name: Vec<u16> }`，多个名称可指向同一对象 | parent/object 必须可解析到同一来源；共享 EntryId 的稳定性及分配方尚未决定 |
| 原始名称 | 不做 Unicode 转换的 UTF-16 码元，包含未配对 surrogate；名称是组件而非完整路径 | 建议按平台保留 Windows UTF-16／Linux bytes；显示、匹配编码与原始身份分离，拒绝无法支持的转换而不静默替换 |
| 原生元数据 | 条目保存 Windows attributes；同对象属性须一致 | attributes 的 u32 位含义不可跨平台复制；共享最低元数据与 reparse／symlink 表示方式待定 |
| 根与目录图 | root 是隐式节点，不作为普通搜索条目；非 reparse 目录对象只允许一个名称关系 | 根是否参与对象／搜索结果、目录别名或挂载关系如何表达由双方决定，不能把私有目录树限制当作所有 provider 的通用事实 |
| 搜索路径 | 从 root 和 parent/name 关系派生；所有范围内硬链接名称分别形成路径 | 完整路径可作派生查询缓存，不能作对象唯一主键；路径分隔与组件合法性由平台解释 |

同一目录下 `(parent, raw name)` 只能对应一个目录项，包含对象身份的关系键也必须满足这一命名空间约束。目前私有 EntryId 在 rename 后改变；这不等于共享 EntryId 已决定采用同样生命周期。

具体对齐例子：对象 O 在目录 A 下名为 `x`、在 B 下名为 `y`。删除 `A/x` 只删除一个目录项，不能删除仍由 `B/y` 命名的 O。把 B 改名为 C 后，O 的对象身份不变，所有后代路径由新的祖先关系派生。若 root 自身被另一个目录替换，不能因为完整路径字符串相同就继续沿用旧 checkpoint。

Windows ADS 在本原型中不作为普通名称目录项；reparse 条目保留，但不遍历目标。Linux symlink 与冒号名称的处理须由对应平台确认，不能套用 Windows 的组件校验。

## 游标与能力

Windows checkpoint 具有 `volume_serial: u64`、`journal_id: u64`、`cursor: i64`；持久容器还绑定 GUID、root、scope 和库存。相同数值的 cursor 只有在相同来源与 journal ID 内才有含义。

共享建议是 provider-tagged、versioned 的平台游标载荷，并保留“没有持久恢复游标”这一可能。Windows USN 可以在保留范围内处理离线变化；Linux 的具体 provider 是否支持持久重放，须由其实现和实测决定。不能要求所有来源提供 USN、全局序号或跨重启重放。没有持久重放的来源应明确要求重启校正。

应分别表达持久重放、稳定对象身份、完整硬链接发现、名称表示及丢失检测能力；不能从“能监听”推导上述全部能力。当前 Windows 原型实际原生身份接口支持完整 64-bit reference；V3 解码可保留 128-bit，但高 64 位非零的原生对象或 parent 明确 Unsupported，V4／未知版本亦拒绝。API 宣告支持版本与实际解码／命名空间定位能力必须分开。

## 私有生命周期与适配边界

| 私有操作 | 当前行为 | 共享装配需要保留或决定的契约 |
| --- | --- | --- |
| `build(root, storage, cancel)` | 记录 journal 边界，目录扫描，重放到已观察的安静边界，验证并 `save_new`；拒绝覆盖已有库 | 仅完整候选可发布；首扫与事件源安装／边界获取顺序由平台承担；新库与替换旧库的授权／模式要明确 |
| `open(root, storage, cancel)` | 加载、校验来源与范围，同步成功后返回；不会自动保存新游标 | 不得把重开成功等同于已持久保存恢复结果；缺失库、损坏库和不可重放需可区分 |
| `sync(cancel)` | 在候选副本校正，成功更换内存库存／cursor 并 Ready；失败保留旧库存／cursor 并 Pending | 候选验证与发布是一个边界；持续变化或超限不能发布部分结果。共享引擎是否使用租约／generation 由核心决定 |
| `query(raw_needle, limit)` | 对最近库存派生完整路径后匹配，返回完整命中数、截断输出及当前 Ready／Pending／Stopped | 查询本身不隐式同步；结果遍历完整性、历史视图状态和持久状态应分开，限制输出不等于只统计前 N 条 |
| `save()` | 仅 Ready 可保存；库存＋来源／范围＋cursor 同次原子替换；保存失败返回原始错误 | 保存失败保留旧磁盘库存，但不回滚已成功 sync 的内存；共享接口须区分已观察 cursor 与已持久 cursor |
| `stop()`／Drop | 释放卷和固定目录句柄；stop 后历史查询标 Stopped，sync 被拒绝 | 停止应幂等，不依赖下一次文件变化；释放前确认内核不再使用异步内存，停止后的结果不可标为持续监控 |

Ready 仅指最近成功观察截止点，文件系统可以立即继续变化。当前原型没有后台线程，没有共享查询租约／generation，也没有任意并发写入下的文件系统事务快照。

当前 `query` 是大小写敏感的原始 UTF-16 字面子串；完整路径及所有匹配会先在内存中形成集合。旧共享引擎的大小写不敏感 AND／`ext:` 语义不能直接由该函数替代。建议原始名称始终保留，公共查询解析及匹配规则由核心独立规定；非 UTF-8 名称是否支持精确原始查询、显示占位或明确 Unsupported 必须决定，不允许有损身份归并。

## 数据交接建议

建议共享边界接收经过平台校正和验证的命名空间候选，而不是把原始 USN Record 当作完整目录项变更。USN 名称是失效提示，硬链接需全名称查询与目录关系共同校正。Linux 的原始通知也应按其 provider 的可靠性规则处理。

候选应携带来源／范围身份、对应平台 cursor（若可用）、命名空间关系和完整性状态；候选数据与 cursor 应来自同一次观察。平台负责原生句柄、权限、事件解释、对象定位和范围校验；核心负责公共查询、存储事务、候选发布及对外状态。窗口内 I/O 失败、loss、取消或预算失败不能作为“空更新成功”。

这是职责建议，尚未选定具体 trait。数据交接可采用完整候选或受事务约束的有界关系批次；全量 Vec、BTreeMap 克隆、分块提交、取消与背压预算需要核心确认，不能把阶段 B 的全量内存实现冻结为产品接口。现有 `EventSource::poll/stop` 路径事件契约继续保留，新增产品后端 seam 的名称／并存方式待定。

持久提交应同时覆盖命名空间、来源／范围身份和平台 cursor；单独提前保存 cursor 会导致重开漏变更。候选在内存发布和磁盘提交之间的先后、失败后对读者的可见状态需要核心明确。阶段 B 只证明同一文件的原子更新与失败保留旧文件，未证明真实掉电一致性。

## 错误、状态与降级

当前返回 `io::Error`，保留 `kind` 与实际 OS code，部分重建原因用消息区分；没有结构化公共 RebuildReason。下表为建议分类，不能描述为已存在的公共错误枚举。

| 原因 | 当前 Windows 行为／证据 | 共享要求建议 |
| --- | --- | --- |
| 来源／root／scope 改变 | 拒绝旧 checkpoint；root 身份逐次核对 | 明确范围失效／要求重建，不能继续以旧身份发布 |
| journal ID 改变、cursor 不再保留或超过末尾 | 明确要求重建，注入校验通过 | 保留具体原因；真实回卷／重建仍未操作验证 |
| 权限／不可读 | 实际普通 token OS 1／5，管理员成功 | 保留 OS code、失败操作与状态；由调用方选择降级，不静默提权 |
| 未知记录／不支持身份宽度 | Unsupported；整批拒绝 | 不转换为空批次，不截断身份 |
| 原生路径竞争 | OS 2／3 有界退避重试；实际六次后收敛 | 允许有限重新校正，跨重试总期限／记录预算累计 |
| 损坏、图冲突、预算超限 | 拒绝候选／保存，不发布新完整状态 | 区分 rebuild、budget 与 unreadable；旧历史结果携带无效当前观察状态 |
| 取消／停止 | Interrupted／Stopped 后 BrokenPipe；旧库存保留 | 取消、停止和持久提交失败不能混为相同“成功空结果” |

Pending 表示本轮新观察不可发布，旧数据仍可作为明确标记的历史结果。当前 `save` I/O 失败不将 Ready 改成 Pending：Ready 描述内存观察，保存结果单独失败。装配时不能用一个布尔值同时表示观察有效、遍历完整、监控运行和持久保存成功。

普通权限／非 NTFS 的建议降级为目录扫描＋ReadDirectoryChangesW，启动和丢失后需校正；它不提供与 USN 等价的持久游标或离线覆盖。是否自动选择降级及如何展示范围不完整由核心决定。本轮没有实现第二套通用后端，也没有实测非 NTFS。

## 双方可直接回复的对齐清单

Windows 状态为“有实现／证据支持该提议”时，也不等于双方 accepted。Linux／核心栏等待对应会话填答。

| 编号 | 需要决定的问题 | Windows 建议／事实 | Linux／核心回复 |
| --- | --- | --- | --- |
| A1 | 来源与 scope 是否分离；重叠范围如何标识和去重？ | 分离来源和范围；checkpoint 仍绑定完整 scope | 待确认：来源身份方案、范围规则 |
| A2 | EntryId 是否跨 rename 稳定，谁分配，删除重建如何处理？ | 保留 object 与关系分离；私有 tuple 键随 rename 改变 | 待确认：生命周期、编码／所有权 |
| A3 | 原始名称、对象身份及最低元数据的带标签表示是什么？ | 保真 UTF-16；原生完整 64-bit reference；attributes 不直接公共化 | 待确认：Linux bytes／身份方案／symlink／复用证据，root 条目及目录／挂载别名模型 |
| A4 | 游标是否可缺失，如何表达 offline replay 与 capability？ | provider-tagged cursor；非持久通知来源可缺失游标并重启校正 | 待确认：实际 Linux provider 能力与恢复证据 |
| A5 | 候选／批次、验证、发布和持久提交由谁负责？ | 平台校正；核心事务与查询；库存与 cursor 同次持久提交 | 待确认：seam、批次／背压、generation／租约、错误状态 |
| A6 | 公共查询及降级语义如何统一？ | 保留原始名称；查询匹配单独规定；降级与覆盖限制显式 | 待确认：AND／ext／大小写／原始查询与自动降级策略 |

上述六项存在未决项时可以继续各自平台实验，但不能修改 ADR 为 accepted 或据此冻结共享引擎接口。双方回复可追加到本文或 ADR；记录对应实现提交及证据，避免仅以平台通常能力作答。

## 共享装配前的必要验收场景

1. 同来源不同 root 的同一对象、不同来源相同数值对象、root 替换均不混淆；来源与 scope 改变拒绝旧 checkpoint。
2. 同对象两个硬链接路径都可搜索；删除一个不删除另一个；目录 rename 改变后代派生路径；目录移出保留仍在范围内的别名。
3. Windows 未配对 UTF-16 与 Linux 非 UTF-8 名称分别保真；展示／查询转换失败不会改变身份或静默遗漏；ADS／reparse／symlink 规则由平台验证。
4. 初次观察边界、并发扫描、重放与发布一致；完整路径集合独立验证；持续变化、取消、loss 或超限保留旧历史视图且拒绝部分完整发布。
5. 离线变化按实际 provider 能力恢复；无持久游标的 provider 明确校正。库存／cursor 提交失败、格式错误和身份变化不能跳过记录。
6. Ready／Pending／Stopped、查询是否遍历完整、观察 cursor 与持久 cursor 分别可见；停止／Drop 安全释放；内存／I/O／句柄预算按真实工作量验证。

这些是建议共享验收条件。阶段 B 已通过其中的小型 Windows 场景，不代表 Linux 或共享装配全部通过；100k／1M、长期压力和真实内核失效的后续预算见阶段 B 报告。

## Linux 当前实现回复（2026-10-03）

以上 Windows 基线和“待确认”栏保留为提出交接时的记录；以下逐项回应 A1–A6，描述已实现的 Linux 行为及仍需双方决定的部分。`origin/main` 的 Windows 原型和交接材料已在集成提交 `5639efea5592c4e001b7244628b1e597261f6be9` 合入 Linux 分支；该次合并没有改变根项目的 `src/`、`tests/` 或 Cargo 输入。合入原型不等于完成公共 Engine 的 NTFS 产品装配。

Linux 的依据是 [已接受的目录项库存 ADR 及其后续扩展](adr/0001-linux-scale-entry-inventory.md)、[规格](../.scratch/linux-million-search/spec.md)、[实施地图](../.scratch/linux-million-search/map.md)和下列源码／公开行为回归。Linux scale 生产基线为 `0c2df04b6f2a810b9c3f16ec7611d1552e83ef07`；`52446d5922b08bf68bcee799a553dd33965798e6` 仅调整 opt-in 100k 原生功能回归的共享等待期限，未改生产实现或正式性能门槛。以下回复没有接受或冻结 [来源／对象／游标 ADR](adr/0001-source-object-entry-cursor.md)，其状态仍为 proposed。

### A1：来源与搜索范围

当前 Linux scale 是单根 Engine，固定 root 描述符、原始根路径和观察到的设备／inode 身份，并绑定根的 mount ID 与显式相对子树排除配置。checkpoint 校验来源和范围；根替换、根重新挂载或不兼容范围不能沿用旧库并发布 Validated。根节点参与内部关系图，但不是普通搜索结果。

嵌套的不同 mount ID 只保留挂载边界目录项，不遍历内容；相同 mount ID 的 Btrfs 子卷可继续遍历，不因设备号不同就误判为另一挂载。局部目录扫描也通过新打开的描述符核对 mount ID。依据：[范围实现](../src/engine/scale/scope.rs)、[checkpoint](../src/engine/scale/checkpoint.rs)、[真实范围回归](../tests/linux_scale_scope.rs)。

目前没有把 SourceId／ScopeId 抽成跨平台公共类型，也没有跨 root 合成查询或重叠范围去重。物理对象来源身份与搜索范围在概念上需要区分；其共享编码和跨 root 归并规则仍待决定，不能将单根设备／inode 绑定直接当成全卷永久 SourceId。

### A2：目录项身份与名称关系

Linux writer 分配 epoch 内单调增长的私有 `u32 EntryId`；概念上的 `EntryKey` 是父目录项 ID 与原始 basename 字节。实际 lookup 保存父 ID 和完整名称 hash，碰撞再比较原始字节，不能只以 hash 判断同一名称。观察到的设备／inode 是物理身份线索，不能证明离线变化后还是同一对象。

已知、可靠的范围内 rename 保持原目录项 ID，目录 rename 更新父／名称关系，后代路径从关系派生。硬链接分别拥有目录项和搜索路径；删除一个名称不删除另一个。删除的 ID 不在同一 epoch 内复用；全量校正或压缩可重分配 ID，旧租约仍使用自己的不可变关系图。因此这些私有 ID 不作为跨 epoch／跨平台持久对象键。

依据：[库存](../src/engine/scale/inventory.rs)、[公共 Engine 硬链接／rename／旧租约回归](../tests/linux_scale_engine.rs)，初始实现 `60d9db0`。Windows tuple 身份随 rename 改变与 Linux epoch 内 ID 稳定是各自实现事实；共享 ID 的所有权、编码和生命周期尚未统一。

### A3：原始名称、元数据与链接

Linux basename、关系查找和持久库保留原始 bytes，结果路径保留 OS 字符串，CLI 可用 NUL 分隔导出原始字节。不同的非 UTF-8 名称不会因显示替换字符而合并。Linux 冒号是普通名称字节，不套用 Windows ADS 规则；当前没有可直接承载 Windows 未配对 UTF-16 的公共 tagged RawName 实现。

库存记录目录项 kind 和观察到的设备／inode；symlink（包括悬空链接）是可搜索目录项，扫描不跟随其目标。公共 scale 快照可返回 `EntryKind`，查询该历史元数据不重新读取当前文件系统。挂载边界采用 A1 的单根规则，不把 Windows attributes 位当成相同元数据。

依据：[原始名称／链接／范围回归](../tests/linux_scale_scope.rs)、[公共租约接口](../src/engine.rs)及 `f90febc`。跨平台 RawName 标签、可选物理身份、最低元数据及 inode 复用的强身份方案仍需单独决定，现有 Linux 观察值不宣称具有 NTFS reference 的离线语义。

### A4：事件来源与恢复游标

现有 Linux 来源是 inotify，没有持久 cursor，也没有跨停机事件重放。重开有效 checkpoint 时先给出明确 Pending 的历史可查询视图，再安装监听并校正；每个目录先 watch 后 enumeration，批次间及最终发布前排空来源并核对观察 generation。不能将数据库加载成功等同于当前目录已完整验证。

可靠的普通文件变化及已知父目录下的新建目录可有界局部更新；新目录同样先 watch 后枚举，不无条件增加 full-root scan。未配对 moved-in 目录、未知目录身份、loss／实际 `IN_Q_OVERFLOW`、覆盖或挂载变化走保守校正／失败路径。持续变化、取消或预算失败保留旧查询，不能发布部分 Validated。

依据：[inotify 来源](../src/linux_events.rs)、[scale 调度](../src/engine/scale.rs)、[stale 启动回归](../tests/linux_stale_startup.rs)、[真实溢出与恢复回归](../tests/linux_scale_recovery.rs)及 `ee2e9f3`／`f3097a4`。共享游标必须允许缺失；Linux 当前只能声明监听、丢失检测和重启校正，不能声明 USN 式 offline replay。

### A5：验证、发布、租约与保存

当前公共 seam 仍是 `Engine` 与 `EventSource::poll/stop`；Linux 的 inventory、扫描和 snapshot store 是私有实现，没有已冻结的共享候选 trait。完整候选必须通过范围、覆盖与 generation 校验才能发布新不可变 cut。目录移动后的整子树深度／路径预算在 writer 修改前校验；失败保留旧查询并报告受影响范围。

查询租约固定 snapshot，分页 cursor 同时绑定该 snapshot 与完整查询文本；cursor 本身不持有租约。最多八个租约、一个退休 cut，读者仍占用退休 cut 时暂停新发布。旧租约保留先前路径，但不被标成已针对新的观察版本验证。查询遍历是否完整、历史观察状态、监控状态和保存结果分别表达。

scale checkpoint 保存同一已发布 cut 的关系图、原始名称、版本／epoch、来源和范围；Linux 没有另行保存的来源恢复 cursor。公共 `save` 要求当前观察 Validated 且没有覆盖缺口，固定数据库 parent 描述符后原子保存。保存失败保留旧数据库并返回错误，不回滚已经成功发布的内存观察。来源、预算和覆盖错误按产生边界的类别处理，错误字符串只供显示。

依据：[snapshot／租约](../src/engine/scale/query.rs)、[公共保存入口](../src/engine.rs)、[checkpoint](../src/engine/scale/checkpoint.rs)、[真实 parent 替换／保存失败回归](../tests/linux_scale_checkpoint.rs)、[monitor 释放回归](../tests/linux_monitor_owner.rs)。候选批次、平台事务与共享核心的责任分界仍需设计；不能把现有路径事件或 Windows 全量容器直接冻结为共享 ABI。

### A6：查询与降级

公共查询文本是 UTF-8：空白分隔普通项作 AND，`ext:` 作扩展名过滤，匹配采用 Unicode 小写归一化。Linux 非 UTF-8 路径按有效 UTF-8 片段匹配，单个词不能跨无效字节，不同 AND 词可命中不同片段；原始路径身份始终保留。当前没有任意原始 bytes 查询或 Windows UTF-16 查询装配。

分页、完整导出、精确 count 和可选排序针对固定 snapshot，不用“输出前 N 条”代替完整性判定。依据：[共享 matcher](../src/index.rs)、[scale 查询](../src/engine/scale/query.rs)、[原始名称回归](../tests/linux_scale_scope.rs)、[公共查询任务回归](../tests/linux_query_jobs.rs)及 `f3986dd`。Windows 当前大小写敏感的字面 UTF-16 查询不能直接替代这个公共 matcher。

Linux scale 是显式 opt-in；Windows 公共 Engine 当前拒绝该模式为 Unsupported，保留已有 bounded 行为。独立 NTFS probes 尚未接入公共 Engine，也没有实现自动切换到 ReadDirectoryChangesW 的产品降级。共同匹配编码、原始精确查询和降级策略仍待决定。

### 本次回复的验收边界

`0c2df04b` 的真实 ext4 证明包含 76 个 native jobs（无环境 skip）及百万测量全部 12 项数值检查通过，原始工件保留在 `/workspace/loci-native-final-ext4-0c2df04b/`。四次维护后 RSS 为 177.059／177.641／177.605／177.609 MiB，HWM 为 367.461 MiB；这属于该源码与该环境的真实证明，不能重贴为本次合并提交的最终 gate，也不能替代 SSD/NVMe 参考硬件或真实 24 小时验收。

旧 `52446d59` 的实际 ext4 76 jobs 已通过，Btrfs debug／release 76 jobs 与 driver correctness 已 exit 0；后续百万阶段仍在运行，属于旧 compiled-source 的开发证明。原始目录为 `/workspace/loci-native-final-ext4-52446d59-focused/` 与 `/workspace/loci-btrfs-final-run-52446d59/`，不据此宣告合并后源码完成最终验收。Linux issues 14–18 保留未完成状态；Windows 证据沿用本文上方的独立原型报告，共享产品装配、最终统一 review 与未验证环境门槛继续保留。
