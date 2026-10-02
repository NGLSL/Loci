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
