# 前置任务：Linux 原生目录替换与数据库保存边界修复

Task: LOCI-LINUX-001-01
Type: task
Status: resolved
Triage: ready-for-agent
Parent: ../spec.md
Baseline: e204917
Validation source: validation/linux-engine-regressions-20261002 / 104b218
Repair branch: codex/linux-engine-regressions-fix
Repair code: 0c4fb7d439bd3b2d473d36804251ef4ea83b6bb0
Integration verification: Windows native / ext4 / Btrfs pending; not merged into main

## Problem Statement

百万级改造之前必须先修复现有原生引擎漏子树却发布 Validated，以及保存数据库跟随被替换父路径进入监控根的问题。验证分支只有失败测试，不能只合测试而保留缺陷。

## Solution

在独立修复分支保留两项原生公开 Engine 回归，并修复目录替换时的库存/监听生命周期，以及 Linux 保存时父目录身份、位置和目录相对操作。补充成功保存的真实新库存检查、同 inode 目录移入 root 的拒绝、目录相对写入/失败清理和新增句柄释放的证据。

## Acceptance Criteria

- 原始两项回归在修复前明确失败，修复后 debug/release 均通过；不 ignore 或倒转断言。
- P1 不允许先出现错误 Validated 再等待周期扫描修复；替换已有子项及后续新子项都完整可查。
- P2 拒绝时旧库保留；成功时新库存确实保存至原选定目录；不能创建数据库或临时文件到重定向目录。
- 保存阶段的创建、替换、清理与父目录同步绑定同一 fd；公开说明“选定目录本身在保存中被搬入 root”这一不支持的并发情况。
- stop/drop 释放新数据库父目录 fd，已有 Windows 平台路径保持兼容；Linux 全量与静态检查通过。
- Windows 交叉类型检查与 Windows 原生运行分开记录；缺少原生 runner 不能冒充已验收。
- 本任务不合 main、不正式 release，不把小夹具结果当百万级、其他文件系统或掉电验收。

## Testing Decisions

以公开 Engine 原生回归为主；已有 Snapshot 公开加载核对保存内容。一个存储级测试明确只证明选定目录 fd 的保存/失败清理绑定，不声称已观测所有并发竞态。完整 Linux debug/release 检查保留真实新 Engine kernel overflow 的执行。

## Comments

- 初始审查：Standards 0 项可行动发现；Spec 1 项 P2 成功保存未验证实际库存的覆盖缺口，已增强断言。
- Red：最新主分支 + 验证提交，在 Linux debug 下两项均失败；补充 P2 断言后仍两项失败，退出 101。
- 完整验证：精确代码提交 0c4fb7d，在 x86_64 Linux 6.18.44 / overlayfs / Rust 1.99.0 上 debug 与 release 各 124 passed、0 failed、6 ignored；新 Engine 的真实 IN_Q_OVERFLOW 在两轮均观测并恢复。
- fmt、Linux all-targets、Linux linux-ffi-check、Windows GNU all-targets 交叉检查及对应 linux-ffi-check 均通过。Windows 原生运行没有执行，不把交叉类型检查算作原生验收。
- 最终两轴审查：Standards 0、Spec 0；之前 P2 成功保存的覆盖缺口已补。原始两项回归、新增同 inode 位置变更回归、实际数据库 fd 释放、存储级绑定/清理均通过。
- Linux 范围内修复已完成；对同一修复的 Windows 原生、ext4/Btrfs 和长期运行验收留作合入/发布门槛，代码不自动合 main 或 release。
- 原始可复现日志、命令和精确 SHA 在本地证据目录 [summary.json](../evidence/native-regressions/summary.json)；证据同样按项目规则保留在本地。
