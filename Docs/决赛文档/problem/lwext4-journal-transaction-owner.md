# lwext4 journal 事务跨任务误解锁

## 背景

Ya2yOS 将 lwext4 的 C 侧资源锁接入 Rust 的 task-aware `TaskRwLock`。journal
transaction 使用 `journal_lock` 串行化，并由 `journal_trans_depth` 表示同一事务的嵌套层数。
该计数位于 `struct ext4_fs`，因此是锁保护下的共享状态，而不是某个 task 私有的状态。

## 现象

在目录递归删除或错误清理路径中，某一轮操作可能在 `ext4_trans_start()` 成功之前结束或跳转，
但旧代码仍会无条件调用 `ext4_trans_stop()` 或 `ext4_trans_abort()`。旧的
`journal_lock_held` 是单个全局布尔值，无法判断当前调用者是否就是持有 journal write lock
的 task；因此错误路径有机会消耗另一个 task 正在进行的 transaction depth，或释放其 journal
write lock。

`ext4_trunc_inode()` 还曾为避开 raw non-recursive lock 而在持有 namespace/inode 资源时拆分并
重建事务。这会在资源锁之间交出 journal lock，扩大与其它 transaction 形成锁循环的窗口。

## 分析

transaction start/stop/abort 必须以 journal write lock 的实际 owner 为边界：只有 owner 才能修改
`journal_trans_depth`、提交/abort journal，或最终解锁。仅凭“当前有非零 depth”或全局布尔值不能
证明所有权，因为其它 hart 可以在该状态存在时进入相同的清理分支。

内核路径安装了 Rust hook，`TaskRwLockState.writer` 保存 write owner 的 tid；C 侧可通过回调查询
“当前 task 是否拥有该锁”。没有 hook 的 standalone/host 串行 fallback 不具备 task identity，因而以
递归 `writer_depth` 保留原先的 serial nested-write 语义。

## 根因

1. 把受 journal lock 保护的共享嵌套深度误当成当前 task 的事务状态。
2. `ext4_dir_rm()` 没有记录本轮循环是否真的成功启动 transaction，清理路径可能收尾一个未启动的事务。
3. 旧的 `journal_lock_held` 只能表达“有人持锁”，不能表达“谁持锁”。

## 修复

- 在 `ext4_fs_rwlock` hook 接口中增加 `write_owned_by_current`，由 Rust
  `lwext4_write_lock_owned_by_current()` 查询对应 `TaskRwLock` 的 writer tid。
- `ext4_trans_stop()` 与 `ext4_trans_abort()` 在处理 depth 和解锁前先确认调用者拥有
  `journal_lock` 的 write side；非 owner 直接返回，不影响其它 task 的 transaction。
- `ext4_dir_rm()` 使用局部 `trans_started`，仅在本轮 `ext4_trans_start()` 成功后执行 stop/abort。
- 删除 `journal_lock_held`；无 hook fallback 增加 `writer_depth`，以支持串行模式的递归 write section。
- `ext4_trunc_inode()` 依赖 task-aware recursive write owner，不再为了模拟嵌套而释放和恢复 journal
  transaction，从而避免在 namespace/inode 资源锁仍被持有时制造新的锁序窗口。

## 涉及文件

- `crates/lwext4_rust/c/lwext4/include/ext4_fs.h`
- `crates/lwext4_rust/c/lwext4/src/ext4.c`
- `crates/lwext4_rust/c/lwext4/src/ext4_fs.c`
- `crates/lwext4_rust/src/blockdev.rs`
- `os/src/fs/ext4_lw/mod.rs`

## 验证

已执行 `rustfmt --edition 2021` 和 `git diff --check`，均通过。执行
`make TARGET_ARCH=riscv64` 时，user build 已完成，但 lwext4 的 CMake 配置因宿主环境缺少
`riscv64-linux-musl-cc` 停止，故未产生可运行的新内核。

尚未完成使用新代码的 BuildStorm、文件系统 LTP、`e2fsck -fn`、动态并发删除压力及 LoongArch64
运行验证；本记录不将该修复表述为已完成运行时回归。
