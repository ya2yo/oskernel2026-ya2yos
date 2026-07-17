# LTP mmap08 文件映射 fd 错误优先级修复

## 背景

LTP `mmap08` 验证文件映射请求使用无效 fd 时的 Linux errno。测试在 setup 中打开后关闭临时文件，并以关闭后的 fd 调用 `mmap()`；同时测试使用的 `page_sz` 保持为 0，用于覆盖多个错误条件同时出现时的错误优先级。

## 现象

初始 `log.ans` 中 musl 和 glibc 的 `mmap08` 均失败：

```
mmap(NULL, page_sz, PROT_WRITE, MAP_FILE | MAP_SHARED, fd, 0)
expected EBADF: EINVAL (22)
```

## 分析

RISC-V debug QEMU trace 显示失败调用实际进入内核时参数为 `len=0`、`flags=MAP_SHARED`、`fd=-1`。`sys_mmap()` 在解析 flags 后立即检查 `len <= 0`，因此返回 `EINVAL`，尚未执行原有的非匿名映射 `fd == -1` 检查。

Linux 对该文件映射请求优先报告无效文件描述符，LTP 因而要求 `EBADF`。匿名映射的 fd 被忽略，仍应保持零长度返回 `EINVAL`。

## 根因

`sys_mmap()` 的长度校验早于非匿名映射的无效 fd 校验，错误地让 `EINVAL` 覆盖了应优先暴露的 `EBADF`。

## 修复

在 `sys_mmap()` 中先解析 mmap flags；对不含 `MAP_ANONYMOUS` 且 `fd == -1` 的请求立即返回 `EBADF`，随后再进行长度校验。匿名映射不进入该 fd 分支，保持既有长度语义。

## 涉及文件

- `os/src/syscall/mm/mmap.rs`

## 验证

- 根目录 `make` 完成 RISC-V 与 LoongArch64 release 构建。
- `make log` 完成 RISC-V debug 构建。
- 在允许 QEMU 写入 `/var/tmp` 的环境中运行 `timeout 80s make run`；RISC-V musl 与 glibc `mmap08` 均输出 `TPASS`，关键调用 `len=0, fd=-1` 返回 `EBADF`，并正常 `shutdown!`。
- LoongArch64 未运行该单测。
