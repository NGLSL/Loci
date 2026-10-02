# Triage labels

本地任务文件使用默认状态标签。`triage` 根据下表填写文件顶部的 `Status:`。

| 技能中的角色 | 本地 `Status:` 值 | 含义 |
| --- | --- | --- |
| `needs-triage` | `needs-triage` | 等待维护者评估 |
| `needs-info` | `needs-info` | 等待补充信息 |
| `ready-for-agent` | `ready-for-agent` | 需求完整，可交给 Agent 实施 |
| `ready-for-human` | `ready-for-human` | 需要人工处理 |
| `wontfix` | `wontfix` | 不予实施 |

分类记录在独立的 `Category:` 行，使用 `bug` 或 `enhancement`。每个分流中的任务使用一个分类和一个状态。

实施任务完成后按 `issue-tracker.md` 记录 `Status: done`。Wayfinder 决策任务的 `open`、`claimed`、`resolved` 同样按该文件管理，不使用本表的分流状态。

修改标签词汇时更新本表，保持角色与本地值的映射一致。
