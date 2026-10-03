# P2：Windows ADS 事件路径拒绝
Status: done
Category: bug
Blocked by: None

## Scope

Windows显式事件中的ASCII冒号路径可能指向NTFS alternate data stream，其metadata可访问但不是目录entry。对Refresh／Remove及Rename两端先纯校验，再做该路径filesystem I/O；拒绝后保留旧视图并安排有界校正。冒号拒绝仅在cfg(windows)编译，Linux合法冒号文件名语义保留。

原始代码提交`277271a37581ee388cc88b3c0086b067928c43c5`已在main。共用批次预检先校验整批路径，再执行P1的依赖rename检查，避免P1提前返回而跳过ADS拒绝。

## Acceptance

- [x] Windows真实NTFS夹具确认stream可stat，read_dir只列出主文件。
- [x] Refresh／Remove／Rename任一端含ASCII冒号都在该路径metadata I/O前拒绝。
- [x] 拒绝保持旧查询内容并标Failed，后续有界校正恢复正常目录entry集合。
- [x] ADS事件先于父目录rename的组合批次也在inspect前拒绝。
- [x] 冒号拒绝代码限于cfg(windows)，Linux合法冒号回归保留；不宣称本轮执行过Linux内核测试。

## Verification

合并树`94740f7576888ddccae01cbb6ef98c744ce7c04a`，Windows debug/release各96 passed、0 failed、2 ignored。两个直接回归：`windows_stream_paths_are_rejected_before_io_and_recovery_preserves_directory_entries`、`windows_ads_before_parent_rename_is_rejected_before_batch_precheck_io`；Engine整批纯校验回归也通过。

定向命令：`cargo +1.99.0 test --offline --locked --test incremental windows_stream_paths -- --nocapture`，以及同一命令的`windows_ads_before_parent_rename`过滤。Linux的`linux_colon_filename`当前未在本机原生执行；Windows linux-ffi-check不替代该证明。

## Comments

2026-10-03：核对历史发现代码已修复，补齐本地issue及当前Windows实际通过范围。没有修改共享源文件或公共契约。
