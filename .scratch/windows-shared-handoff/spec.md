# Windows／共享核心接口交接整理
Status: done
Category: enhancement
Blocked by: None

## Goal

根据阶段 B 已实测的私有实现，整理可供 Windows 核心装配审阅的身份、名称、同步、查询及持久化契约。当前任务完成条件为交接材料可审阅；公共契约接受 ADR 与主引擎装配属于后续任务。

## Scope

- 工作树及分支沿用本会话的独立阶段 B 分支，不修改 main 或共享 Rust 模块。
- 记录 Windows 当前实现事实、共享建议与待核心决定项，三者明确区分。
- 共享 ADR 保持 proposed；Windows 产品装配与公共契约仍待确认。
- 不新增存储实现，不创建 100k／1M 文件，不运行新的 UAC／USN 验收。

## Acceptance

- [x] 交接文档列出固定实现／实测基线及源码入口。
- [x] 来源与范围、对象与目录项、原始名称与匹配规则、平台游标分别描述。
- [x] 给出 build/open/sync/query/save/stop 的实际语义和共享适配差异。
- [x] 列出错误／取消／历史结果／内存发布与持久保存的边界，不把私有原型能力当共享实现。
- [x] 提供可直接逐项回复的核心契约清单及共享验收场景。
- [x] 检查文档链接、Git 忽略规则与改动范围；无原型代码或共享模块变更。

## Comments

2026-10-03：Windows 阶段 B 历史基线 `8607bb5584b642dafb9e3edc766b78a63b959f66` 的接口材料整理完成。项目现仅支持 Windows，交接文档已移除 Linux 协作和 Linux 产品回复。材料整理 done 不代表公共契约已 accepted，也不代表 NTFS 原型已装配公共 Engine。
