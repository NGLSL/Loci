# P1：依赖目录改名批次校正
Status: done
Category: bug
Blocked by: None

## Scope

较早的子路径事件携带发生时的路径，随后祖先目录改名使最终磁盘路径不同。此类批次保留旧查询版本并标Pending，通过现有有界校正恢复完整路径集合；普通独立rename保留增量快路径。

实现为`src/incremental.rs::requires_reconcile`，在Portable 和 Engine的apply／topology之前调用。原始代码提交`277271a37581ee388cc88b3c0086b067928c43c5`已在main；本次补齐记录及当前Windows验证，不重复实现。

## Acceptance

- [x] 子文件create／rename后父目录rename不发布遗漏的Validated库存，旧版本保持Pending。
- [x] rename源名称复用及Refresh祖先后的替代子树不被错误纳入旧事件。
- [x] 下一次有界校正恢复准确查询路径集合。
- [x] 普通独立目录／文件rename保留增量路径，不增加root full_scans。
- [x] 共用检查在库存apply及watch topology变更前调用，不扩大预算或公共接口。

## Verification

合并树`94740f7576888ddccae01cbb6ef98c744ce7c04a`，Windows debug/release各96 passed、0 failed、2 ignored。`tests/incremental_rename_regressions.rs`四项当前Windows回归通过；`tests/incremental.rs`和`tests/engine.rs`的普通rename快路径回归通过。

定向命令：`cargo +1.99.0 test --offline --locked --test incremental_rename_regressions -- --nocapture`。当前仅保留 Windows 用例；4096条目／128目录上限不变。

## Comments

2026-10-03：核对历史发现代码已修复，旧spec状态滞后。重新验证现有回归后关闭任务并保留实现SHA。

2026-10-03 范围更新：项目仅支持 Windows；Linux 原生实现与测试已移除。历史评论保留为旧提交证据，不构成当前 Linux 要求或本次验证。
