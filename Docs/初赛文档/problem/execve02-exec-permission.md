# execve02: execve 缺少执行权限检查

## 背景

LTP `execve02` 会把资源文件 `execve_child` 改成 `0700`，随后子进程调用 `seteuid(nobody)`，再执行：

```text
execve("execve_child", ["execve_child", NULL], environ)
```

测试期望非 root 的 effective uid 无权执行 root 拥有且仅 owner 可执行的文件，因此 `execve()` 必须失败并返回 `EACCES`。

## 现象

修复前 `log.ans` 中 musl 单测失败为：

```text
execve_child.c:27: TFAIL: execve_child shouldn't be executed

Summary:
passed   0
failed   1
broken   0
```

glibc 路径也进入了不该执行的 child，只是随后因动态链接库路径问题提前失败：

```text
execve_child: error while loading shared libraries: libc.so.6: cannot open shared object file: No such file or directory
tst_test.c:405: TBROK: Invalid child (11) exit value 127
```

这说明内核已经允许 `execve_child` 被加载，权限错误没有在 `execve()` 阶段返回给测试程序。

## 分析

`sys_execve()` 在解析路径后通过：

```text
open(&abs_path, OpenFlags::O_RDONLY, NONE_MODE)
```

读取目标 ELF。通用 `open()` 目前只对写模式打开和创建路径做权限检查，`O_RDONLY` 不检查执行位。因此 `execve()` 只要能只读打开文件，就会继续 `read_all()` 并加载 ELF。

`execve02` 的关键权限变化是 `SAFE_SETEUID(nobody_uid)`。Linux 的执行权限检查使用 effective uid/gid：当 effective uid 不是文件 owner，且 effective gid 也没有对应执行位时，应返回 `EACCES`。本例中 `execve_child` 被改为 `0700`，`nobody` 不能执行。

## 根因

Ya2yOS `execve` 路径缺少目标文件执行权限检查，把“能只读打开 ELF”误当成“能执行 ELF”。这会让非 root 进程执行没有 owner/group/other 对应执行位的文件，导致 LTP `execve02` 进入不应运行的 `execve_child`。

## 修复

在 `os/src/syscall/task/execve.rs` 中新增 `current_task_can_exec()` / `check_exec_permission()`：

- 使用当前任务的 `effective_uid` / `effective_gid`；
- 非 root 按 owner/group/other 类别检查 `S_IXUSR`、`S_IXGRP`、`S_IXOTH`；
- root 仍要求文件至少有任一执行位，避免无执行位普通文件被直接执行；
- 打开目标文件后、读取 ELF 前执行权限检查；
- shebang 脚本转解释器时，对解释器 ELF 也执行同样检查。

涉及文件：

- `os/src/syscall/task/execve.rs`

## 验证

已执行：

```text
cargo fmt --manifest-path os/Cargo.toml --all
make
timeout 120s make run > log.ans 2>&1
```

当前默认 `TARGET_ARCH=loongarch64`，`make` 通过。

复现配置下单跑 musl/glibc `execve02`，两者均通过：

```text
execve02.c:54: TPASS: execve() failed expectedly: EACCES (13)

Summary:
passed   1
failed   0
broken   0
skipped  0
warnings 0
```

`log.ans` 中未再出现 `execve_child shouldn't be executed`。
