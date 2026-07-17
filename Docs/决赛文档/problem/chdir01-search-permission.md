# LTP chdir01 目录 search 权限修复

## 背景

LTP `chdir01` 在 ext2 和 tmpfs 上分别以 root 与 `nobody` 执行 `chdir(2)`，覆盖普通文件、不可搜索目录、可搜索目录、`.`、`..`、根目录、缺失路径及符号链接环。

## 现象

初始 `log.ans` 中 musl 和 glibc 的 ext2、tmpfs 两轮均有同一失败：

```
nobody: chdir("keep_out") returned unexpected value 0: SUCCESS (0)
```

`keep_out` 由 root 以 mode `0644` 创建，root 可以进入，非 root 应返回 `EACCES`。每个 libc 结果均为 `passed 30 failed 2 broken 0`。

## 分析

`sys_chdir()` 读取用户路径并调用 `open()`，随后只检查解析结果是否为目录便设置 cwd。通用 `open()` 目前不对只读打开强制目录 search 权限，因此 effective uid 已由 LTP 切换至 `nobody` 后仍可进入无执行位的目录。

目录路径遍历要求每个目录分量均有 search（执行）权限，不仅是最终目录。项目已有 `mode_allows()`，可按 owner/group/other 和 effective uid/gid 复用该判断。

## 根因

`sys_chdir()` 漏掉了基于 effective credential 的目录 `S_IXUSR/S_IXGRP/S_IXOTH` 检查，导致非 root 跳过目录 search 权限。

## 修复

`sys_chdir()` 在确认目标为目录后读取 task 的 effective uid/gid，并对解析后的目录路径逐级检查 search 权限：

- euid 为 0 时保持 root 绕过权限检查；
- 非 root 使用 `mode_allows()` 选择 owner、group 或 other 执行位；
- 缺少任一目录分量的执行位时返回 `EACCES`；
- 已有的 `ENOTDIR`、`ENOENT`、`ELOOP` 和 cwd 解析路径保存语义不变。

## 涉及文件

- `os/src/syscall/fs/path.rs`

## 验证

- `make log` 完成 RISC-V debug 构建。
- 在允许 QEMU 写入 `/var/tmp` 的环境中运行 `timeout 80s make run`；RISC-V musl、glibc 的 ext2 和 tmpfs 测试均为 `passed 32 failed 0 broken 0 skipped 0 warnings 0`，共 64 条 `TPASS`，其中 `nobody: chdir("keep_out")` 返回 `EACCES`，并正常 `shutdown!`。
- 根目录 `make` 完成 RISC-V 与 LoongArch64 release 构建。
- LoongArch64 未运行 `chdir01` 单测。
