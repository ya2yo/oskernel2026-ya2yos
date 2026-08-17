# fcntl OFD 状态、memfd sealing 与 write-life hint 扩展

## 背景

`fcntl(2)` 的基础命令已经覆盖记录锁、文件租约、异步 I/O owner 和 pipe 容量，仍缺少
Linux 文件描述符模型中几组容易被混淆的状态：每个 fd 的 `FD_CLOEXEC`、共享 open file
description（OFD）的 status flags，以及 memfd sealing 和 write-life hint。用户态库和现代
构建工具会通过 `dup*`、`F_DUPFD_QUERY`、`F_SETFL`、memfd 和这些扩展命令观察这些状态。

## 现象

原实现把所有 flags 放在单个 `FileDescriptor` 位图中，导致 `F_SETFD` 可能影响 dup 出来的
fd，`F_SETFL` 只改变返回值而不改变普通文件实际追加位置，`F_DUPFD_QUERY` 也被误解为
“查询下一个空闲 fd”。memfd 由匿名 `TmpFile` 表示，但 `F_ADD_SEALS/F_GET_SEALS` 没有
对应状态；`F_GET/SET_RW_HINT` 也没有 inode 或 OFD 存储位置。

## 分析

Linux 将 `FD_CLOEXEC` 放在 descriptor table entry，将访问模式和 `O_APPEND/O_NONBLOCK`
等 status flags 放在 open file description。因此 `dup(2)`、`F_DUPFD*` 和 fork 后的 fd
共享 status，而新 fd 的 close-on-exec 位按命令独立复制。Linux 7.0 的 `F_DUPFD_QUERY`
比较两个 fd 是否指向同一个 OFD，返回 1/0；它不是 fd 分配接口。

`F_GET/SET_RW_HINT` 面向底层 inode，`F_GET/SET_FILE_RW_HINT` 面向特定 OFD。memfd seal
是匿名文件对象的单调位图：没有 `MFD_ALLOW_SEALING` 时初始带 `F_SEAL_SEAL`，写、扩展、
收缩和后续添加 seal 必须分别受 seal 阻止。

## 修复

- `FileDescriptor` 拆分 `descriptor_flags` 与共享的 `OpenFileStatus`。前者只保存
  `O_CLOEXEC`，后者保存 `F_GETFL` 可见 flags 与 file rw hint；dup/fork 通过 `Arc` 共享
  status。`F_DUPFD_QUERY` 改为 OFD 指针身份比较。
- `F_SETFL` 同步更新底层 `OSFile`/`TmpFile` 的 append 和 nonblocking 状态。普通文件和
  匿名内存文件在每次 write 前重新定位到 EOF，确保 `O_APPEND` 具有实际写入效果。
- `TmpFile` 增加 memfd 标识、seal 位图和 sealing 策略，支持 `F_SEAL_SEAL`、
  `F_SEAL_SHRINK`、`F_SEAL_GROW`、`F_SEAL_WRITE`、`F_SEAL_FUTURE_WRITE` 与
  `F_SEAL_EXEC` 的查询/添加；写入和 truncate 对适用 seal 返回 `EPERM`。
- ext4 regular inode 增加原子 inode-wide write-life hint；OFD-specific hint 保存在共享
  `OpenFileStatus`。用户指针统一通过 `copy_from_user_val/copy_to_user_val` 访问，设置
  inode hint 时检查 inode owner 或 `CAP_FOWNER`。
- 新增 `user/src/bin/initproc/fcntl_regression.rs`，覆盖 dup/cloexec、OFD 查询、共享
  status flags、memfd append/seals、无 sealing 权限策略和两类 rw hint。

## 边界

- `F_GET/SET_RW_HINT` 当前只支持 ext4 regular file；pipe、socket、memfd 等对象返回
  `EOPNOTSUPP`。FILE variant 按 UAPI 语义提供，独立 open 不共享、dup/fork 共享。
- `F_SEAL_FUTURE_WRITE` 在当前 `TmpFile` 没有共享文件映射路径的前提下与直接写保护
  等价；尚未实现 Linux 对既有 writable mapping 的细分策略。`F_SEAL_EXEC` 可查询并记录，
  但匿名文件当前没有 chmod/exec 权限修改路径可供进一步拦截。
- `F_SETFL` 已支持本轮可变位的状态同步，但尚未补齐 Linux 对 `O_NOATIME` owner/
  `CAP_FOWNER`、`O_DIRECT` 文件类型和 `O_ASYNC` fasync 回调的全部检查。

## 涉及文件

- `os/src/fs/fstruct.rs`
- `os/src/fs/files/os_file.rs`
- `os/src/fs/files/tmp_file.rs`
- `os/src/fs/vfs.rs`
- `os/src/fs/ext4_lw/inode/mod.rs`
- `os/src/fs/ext4_lw/inode/vfs.rs`
- `os/src/syscall/fs/fcntl.rs`
- `os/src/syscall/fs/memfd.rs`
- `os/src/syscall/options.rs`
- `user/src/bin/initproc.rs`
- `user/src/bin/initproc/fcntl_regression.rs`

## 验证

已执行：

```text
git diff --check
make TARGET_ARCH=riscv64
make build-arch TARGET_ARCH=riscv64
timeout 150s make TARGET_ARCH=riscv64 run > /tmp/fcntl-regression-riscv.log 2>&1
```

`make TARGET_ARCH=riscv64` 完成 RISC-V64 与 LoongArch64 release 构建；最后一次 RISC-V
QEMU 定向回归输出 `fcntl regression: PASS` 和 `shutdown!`。LoongArch64 本轮完成构建，
未运行同等 QEMU 用户态回归；完整 LTP/BuildStorm 也未在本轮执行。
