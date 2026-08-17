# fcntl32 写租约与其他打开文件描述符语义修复

## 背景

LTP `fcntl32` 验证 `fcntl(fd, F_SETLEASE, F_WRLCK)` 的排他性。测试对同一普通文件分别使用
`O_RDONLY`、`O_WRONLY` 和 `O_RDWR` 的九种组合打开两个独立文件描述符，然后在第一个描述符上
设置写租约；Linux 要求在仍有其他文件描述符打开时拒绝该请求，测试接受 `EAGAIN` 或 `EBUSY`。

## 现象

修复前的 `2.ans` 中，`fcntl32` 的九个断言都报告写租约意外成功：

```text
fcntl(F_SETLEASE, F_WRLCK) succeeded unexpectedly
```

## 分析

`sys_fcntl()` 的 `F_SETLEASE` 分支原先只解析当前描述符的访问模式，并将请求委托给全局
lease 表。lease 表只记录已经设置的租约，不记录普通文件的打开引用，因此同一进程先后
`open()` 同一文件得到两个描述符时，第二个引用不会阻止第一个描述符设置写租约。

## 根因

写租约的前置条件缺少同一 inode 的其他普通文件描述符检查，导致 syscall 在已有独立打开
引用时错误返回成功。

## 修复

- 在 `FdTable` 增加 `has_other_regular_file_reference()`，在 fd 表读锁内扫描其他槽位。
- 仅将 `FileClass::File` 且 inode `Arc` 身份相同的描述符视为冲突，避免把 pipe、socket 或
  其他抽象文件误判为普通文件引用。
- `F_SETLEASE(F_WRLCK)` 在发现冲突时返回 `EAGAIN`；无冲突请求继续使用既有 lease 表。
- 读取 `F_SETLEASE` 所需的访问模式时使用 open-file-description 可见的 `getfl_flags()`，
  不把仅属于 descriptor 的 `FD_CLOEXEC` 等标志混入判断。

## 涉及文件

- `os/src/fs/fstruct.rs`
- `os/src/syscall/fs/fcntl.rs`

## 验证

已执行：

```text
cargo fmt --manifest-path os/Cargo.toml --check
git diff --check -- os/src/fs/fstruct.rs os/src/syscall/fs/fcntl.rs
make TARGET_ARCH=loongarch64 build-arch
make TARGET_ARCH=riscv64 build-arch
timeout 240s make TARGET_ARCH=loongarch64 run
```

LoongArch64 定向 QEMU 日志 `/tmp/fcntl32-after-lease-fd-check.log` 显示九个断言均为
`TPASS`，每次返回 `EAGAIN/EWOULDBLOCK(11)`；该单项结果为 `RESULT GLIBC LTP SINGLE CASE
fcntl32 : 0`，汇总为 `passed 9 / failed 0 / broken 0`。
