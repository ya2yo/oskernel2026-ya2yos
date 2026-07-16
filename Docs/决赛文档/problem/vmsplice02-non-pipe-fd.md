# LTP vmsplice02 非 pipe fd 错误码修复

## 背景

最新 `log.ans` 中 musl 和 glibc 的 `vmsplice02` 均有一项失败：对有效但非 pipe 的 fd 调用 `vmsplice()` 时，期望 `EBADF`，实际得到 `EINVAL`。

## 分析

`sys_vmsplice()` 已经正确处理了不存在的 fd 和非法 flags。问题发生在 fd 类型检查：调用 `FileDescriptor::pipe()` 失败后，代码把所有错误统一映射为 `EINVAL`。Linux 的 `vmsplice(2)` 目标 fd 必须是 pipe；有效的普通文件、目录或其他非 pipe fd 应按错误 fd 类型返回 `EBADF`。

## 根因

非 pipe fd 的类型错误被错误映射为 `EINVAL`，导致 `vmsplice02` 的 fd 语义断言失败。

## 修复

将 `fd_entry.pipe()` 失败映射为 `EBADF`。不存在的 fd 仍在前面的 fd table 检查中返回 `EBADF`，非法 flags 仍返回 `EINVAL`。

## 涉及文件

- `os/src/syscall/io_mpx/splice.rs`

## 验证

`cargo fmt --manifest-path os/Cargo.toml -- --check` 通过；根目录 `make` 的 RISC-V 和 LoongArch64 构建均通过。`timeout 120s make run` 中 RISC-V musl/glibc 的 `vmsplice02` 均为 `passed 3 failed 0 broken 0 skipped 0 warnings 0`，并正常 `shutdown!`。
