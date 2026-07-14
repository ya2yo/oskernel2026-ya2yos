# splice07 匿名挂载 fd 默认可读写导致阻塞

## 背景

LTP `splice07` 枚举普通文件、pipe、socket、eventfd 和匿名 inode 等文件描述符的无效 `splice(2)` 组合。对于空 pipe 输入端，测试跳过预期会因输入为空而阻塞的少数合法组合；其余组合必须立即返回 `EINVAL` 或 `EBADF`。

`fsopen(2)`、`fspick(2)` 和 `open_tree(2)` 分别在内核中由 `FsContextFd` 与 `DetachedMountFd` 表示。它们是用于 mount API 控制操作的匿名 fd，而不是数据流 I/O 端点。

## 现象

旧 `log.ans` 的最后一条有效日志是：

```text
[syscall begin] Fsopen
[syscall ret --- OK] Fsopen ret = 5
[syscall begin] Splice
[sys_splice] fd_in=3, off_in=0x0, fd_out=5, off_out=0x0, len=1, flags=0.
QEMU: Terminated
```

这里 `fd_in=3` 是空 pipe 的读端，`fd_out=5` 是 `fsopen()` 返回的上下文 fd。系统调用没有返回，QEMU 最终被外部超时终止。

## 分析

此前已在 `sys_splice()` 中按 `fstat().st_mode` 对非 pipe 文件做类型准入：非 pipe 输出端仅接受 `S_IFREG`。然而 `FsContextFd::file_stat()` 为兼容匿名 inode 的 stat 结果返回了 `S_IFREG | 0600`，因此该检查把 fscontext 错认成可写普通文件。

`File` trait 的 `readable()` 与 `writable()` 默认实现均返回 `true`。`FsContextFd` 与 `DetachedMountFd` 只覆写了实际的 `read()`/`write()`，令其返回 `EINVAL`，却没有覆写这两个能力查询。于是 `sys_splice()` 的权限检查也会通过；在通用搬运路径中先调用空 pipe 的 `read()`，该读端仍有写端存在，因而按阻塞语义休眠，永远无法执行到输出 fd 的 `write() -> EINVAL`。

## 根因

挂载 API 的控制型匿名 fd 继承了通用 `File` 的“可读、可写”默认值。`st_mode` 只能描述 stat 视角的文件类别，不能作为控制型匿名 fd 是否可参与数据流 I/O 的唯一依据；与默认 I/O 能力组合后，造成 `splice` 误入空 pipe 的阻塞读取路径。

## 修复

在 `os/src/fs/files/mountfd.rs` 中：

- `FsContextFd` 显式实现 `readable() -> false` 和 `writable() -> false`；
- `DetachedMountFd` 也显式实现相同能力，覆盖 `open_tree()` 产生的 detached mount fd；
- `read()`/`write()` 原有的 `EINVAL` 保持不变，直接调用仍具备稳定错误语义。

这样 `sys_splice()` 在通用读写前的既有能力检查中直接返回 `EBADF`，符合 `splice07` 对不支持组合允许的错误码范围，并且不会破坏 `fsconfig`、`fsmount` 或 `move_mount` 对这两类 fd 的专用访问路径。

## 涉及文件

| 文件 | 修改 |
| --- | --- |
| `os/src/fs/files/mountfd.rs` | 让 filesystem context 与 detached mount fd 明确声明为不可读、不可写 |

## 验证

已执行：

```text
make
```

结果：RISC-V 与 LoongArch64 均构建通过，仅有既有 vendored `smoltcp` warning。

维护者已运行当前单测配置并写入 `log.ans`。日志中：

```text
splice07.c:56: TPASS: splice() on pipe read end -> fsopen : EBADF (9)
splice07.c:56: TPASS: splice() on pipe read end -> fspick : EBADF (9)
splice07.c:56: TPASS: splice() on pipe read end -> open_tree : EBADF (9)

Summary:
passed   566
failed   0
broken   0
skipped  25
warnings 0
...
shutdown!
```

musl 与 glibc 两轮均出现上述 `passed 566 / failed 0 / broken 0` Summary，日志没有 `TFAIL`、`TBROK`、panic 或 `QEMU: Terminated`。`FAIL LTP CASE splice07 : 0` 是 musl 单测包装器对成功退出码 `0` 的既有输出，以 LTP Summary 为准。
