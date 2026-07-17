# LTP chdir04 pathname 长度边界修复

## 背景

LTP `chdir04` 覆盖 `chdir(2)` 的超长 pathname、不存在 pathname 和坏用户指针错误码。Linux 的 pathname 上限包含末尾 NUL；路径在上限内找不到终止 NUL 时必须返回 `ENAMETOOLONG`。

## 现象

初始 `log.ans` 中 musl 和 glibc 的长 pathname 断言均失败：

```
chdir() expected ENAMETOOLONG: ENOENT (2)
```

不存在路径的 `ENOENT` 和坏指针的 `EFAULT` 均已通过，因此每个 libc 的结果为 `passed 2 failed 1 broken 0`。

## 分析

`read_user_cstr()` 使用大小为 `MAX_PATH_LEN`（256）的缓冲区。若前 256 字节未出现 NUL，它返回长度恰为 256 的字符串，供 syscall 层决定错误码。

`sys_chdir()` 使用 `path.len() > MAX_PATH_LEN` 检查长度，漏掉了该精确边界，继而对截断字符串执行路径查找。由于该目录不存在，最终错误码退化为 `ENOENT`。

## 根因

`sys_chdir()` 将最大长度的非终止字符串误视为合法 pathname；没有考虑 `MAX_PATH_LEN` 已包含 C 字符串终止 NUL。

## 修复

将 `sys_chdir()` 的长度检查改为 `path.len() >= MAX_PATH_LEN`。这会在用户字符串未能于 256 字节内终止时直接返回 `ENAMETOOLONG`，同时保留短的不存在路径 `ENOENT` 与坏指针 `EFAULT` 语义。

## 涉及文件

- `os/src/syscall/fs/path.rs`

## 验证

- `make log` 完成 RISC-V debug 构建。
- 在允许 QEMU 写入 `/var/tmp` 的环境中运行 `timeout 80s make run`；RISC-V musl/glibc `chdir04` 均为 `passed 3 failed 0 broken 0 skipped 0 warnings 0`。超长路径返回 `ENAMETOOLONG`，不存在路径返回 `ENOENT`，坏指针返回 `EFAULT`，并正常 `shutdown!`。
- 根目录 `make` 完成 RISC-V 与 LoongArch64 release 构建。
- LoongArch64 未运行 `chdir04` 单测。
