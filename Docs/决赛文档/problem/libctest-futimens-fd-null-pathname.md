# libc-test futimens fd + NULL pathname 语义修复

## 背景

维护者提供的 `log.ans` 只运行 musl 静态 `libc-test` 的
`src/functional/utime.c`。该测例通过 `futimens(2)` 覆盖显式时间、
`UTIME_NOW`、`UTIME_OMIT` 和 `1LL << 32` 时间戳的写回语义。

Linux ABI 中，`futimens(fd, times)` 等价于
`utimensat(fd, NULL, times, 0)`。它与 `utimes(NULL, ...)` 的 pathname
空指针错误路径不同：前者以已打开的 fd 为目标，后者使用
`AT_FDCWD` 且应返回 `EFAULT`。

## 现象

原始 RISC-V `log.ans` 在启动 `entry-static.exe utime` 后，
`utime.c:29` 起的所有有效 fd `futimens()` 断言均失败；最后一次
`tv_sec = 1LL << 32` 的调用打印 `Bad address`。后续 `fstat()` 的
atime/mtime 断言失败是时间戳根本未更新的连锁结果。

同一测例中的 `futimens(-1, ...) -> EBADF` 与
`utimensat(AT_FDCWD, "/dev/null/invalid", ...) -> ENOTDIR` 均已通过，
因此不是 syscall 号、用户 `timespec` 布局或高位时间戳存储的首要问题。

## 分析

`sys_utimensat()` 曾为 LTP `utimes01` 修复将任意 `path == NULL` 直接
返回 `EFAULT`。这保持了 `utimes(NULL, ...)` 的正确语义，却同时拒绝了
musl `futimens()` 的 fd-only ABI；用户态因而在读取 `times` 数组、解析
`UTIME_NOW`/`UTIME_OMIT` 或更新 inode 前就收到了 `EFAULT`。

不能把 NULL pathname 再无条件转换为空字符串：这样会回归
`AT_FDCWD + NULL pathname` 的 `EFAULT` 行为，也会通过 pathname 重开文件，
错误地丢失已 unlink 但仍由 fd 引用的目标语义。

## 根因

`os/src/syscall/fs/ctl/time.rs` 未区分两种 NULL pathname 调用约定：

- `dirfd == AT_FDCWD` 的 pathname API 空指针，必须保留 `EFAULT`；
- 非负有效 fd 的 `futimens` 调用，应直接使用该 fd 所引用的文件。

## 修复

- `path == NULL && dirfd >= 0` 时，从 `proc.fd_table` 获取描述符并直接取得
  `OSFile`/inode，避免 pathname 重解析和 reopen 竞态；
- `O_PATH` 描述符返回 `EBADF`，无效 fd 继续由 fd 表返回 `EBADF`；
- `path == NULL && dirfd == AT_FDCWD` 保留 `EFAULT`；其余负 fd 返回
  `EBADF`，包括 `futimens(-1, ...)`；
- fd 与 pathname 两条分支在取得目标后共用已有的只读挂载、owner、
  `copy_from_user`、`UTIME_NOW`/`UTIME_OMIT` 和 inode 时间戳更新逻辑。

## 涉及文件

- `os/src/syscall/fs/ctl/time.rs`
- `Docs/决赛文档/problem/libctest-futimens-fd-null-pathname.md`
- `Docs/决赛文档/problem/README.md`
- `Docs/决赛文档/开发日志.md`
- `Docs/决赛文档/ai.log`
- `Docs/决赛文档/AI_INTERACTION.md`

## 验证

执行：

```bash
cargo fmt --manifest-path os/Cargo.toml -- --check
make
timeout 120s make TARGET_ARCH=riscv64 run > log.ans 2>&1
timeout 120s make TARGET_ARCH=loongarch64 run > log.ans 2>&1
```

格式检查和 `git diff --check` 通过；`make` 完成 RISC-V 与 LoongArch64
release 构建，仅出现既有 vendored `smoltcp` warnings。两架构定向 QEMU 运行的
`entry-static.exe utime` 均输出 `Pass!` 并正常 `shutdown!`，未再出现
`futimens` 断言、`Bad address`、`FAIL utime`、panic、TFAIL 或 TBROK。

最后一次 LoongArch64 运行的输出保留在仓库根目录 `log.ans`；其中启动阶段的
`WARN Allocated address` 是既有内存分配日志，不是 `utime` 测例失败。
