# LTP writev01 writev 参数与管道错误码修复

## 背景

`log.ans` 中 musl 和 glibc 的 `writev01` 均有 4 项失败，失败项涉及无效 fd、空 iovec、零长度 NULL iovec 和关闭读端后的 pipe 写入。

## 现象

修复前日志为：

- invalid fd：期望 `EBADF`，实际 `EINVAL`；
- zero iovcnt：期望成功返回 0，实际 `EINVAL`；
- NULL and zero length iovec：期望成功写入，实际 `EINVAL`；
- closed pipe：期望 `EPIPE`，实际 `EINVAL`。

`invalid iov_len` 和 `invalid iovcnt` 两项已经通过。

## 分析

`sys_writev()` 在解析 fd 前直接把 `iovcnt == 0` 判为 `EINVAL`，并且把 fd 表越界错误返回为 `EINVAL`。对于每个 iovec，函数总是按 `iov_len` 从 `iov_base` 拷贝用户内存；当 `iov_len == 0` 且 `iov_base == NULL` 时，用户拷贝错误会提前终止 syscall，无法得到 Linux 的空操作语义。

关闭 pipe 的 `EPIPE` 逻辑已经位于 pipe 文件对象的 `write()` 实现中，但上述参数阶段提前返回，因而没有执行到该路径。

## 根因

writev 的参数检查没有遵循 Linux 的 fd 和空 iovec 语义：fd 越界错误码错误，空 iovec 数组被误判为非法，零长度 iovec 被错误地当成需要访问其 base 地址的缓冲区。

## 修复

- fd 超出 fd table 或对应槽为空时返回 `EBADF`；
- fd 和可写性检查完成后，`iovcnt == 0` 返回成功值 0；
- 对每个 iovec 先校验 `iov_len`，零长度项不访问 `iov_base`；
- 保留 pipe 文件对象的 broken-pipe 路径，使关闭读端时返回 `EPIPE`。

## 涉及文件

- `os/src/syscall/io_mpx/file.rs`

## 验证

`cargo fmt --manifest-path os/Cargo.toml -- --check` 通过；根目录 `make` 完成 RISC-V 和 LoongArch64 构建。`timeout 120s make run` 在 RISC-V 上完成，musl 和 glibc 的 `writev01` 均为 `passed 6 failed 0 broken 0 skipped 0 warnings 0`，并正常 `shutdown!`。
