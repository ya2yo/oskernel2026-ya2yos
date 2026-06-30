# execve04: 写打开文件执行未返回 ETXTBSY

## 背景

LTP `execve04` 验证 `execve(2)` 的 `ETXTBSY` 语义：子进程先用 `O_WRONLY` 打开资源文件 `execve_child` 并保持 fd 不关闭，父进程等待 checkpoint 后再执行同一个文件。

Linux 语义要求：当目标可执行文件当前被其他进程以写模式打开时，`execve()` 必须失败并返回 `ETXTBSY`。

## 现象

修复前新的 `log.ans` 中，musl 和 glibc `execve04` 都进入了不应执行的 child：

```text
RUN LTP CASE execve04
execve_child.c:27: TFAIL: execve_child shouldn't be executed

Summary:
passed   0
failed   1
broken   0
```

glibc 单测同样输出：

```text
RUN GLIBC LTP SINGLE CASE execve04
execve_child.c:27: TFAIL: execve_child shouldn't be executed
RESULT GLIBC LTP SINGLE CASE execve04 : 256
```

## 分析

`execve04.c` 的父子流程是：

- 子进程 `SAFE_OPEN(TEST_APP, O_WRONLY)`；
- 子进程通过 checkpoint 通知父进程，此时 fd 仍保持打开；
- 父进程调用 `execve(TEST_APP, argv, environ)`；
- 测试期望 `errno == ETXTBSY`。

Ya2yOS 的 `sys_execve()` 只检查目标文件是否可打开、是否有执行权限、是否为 ELF 或 shebang。普通文件对象 `OSFile` 只保存 `readable/writable` 权限，没有全局记录“某个 inode 当前存在写打开的 open file description”。因此父进程 `execve()` 看不到子进程持有的 `O_WRONLY` fd，继续读取并加载 ELF，最终运行 `execve_child`。

只扫描当前进程 fd 表不能解决这个问题，因为写打开 fd 在另一个进程里。需要在普通文件对象生命周期上记录跨进程的写打开状态。

## 根因

内核缺少普通文件的写打开计数，`execve()` 无法判断目标文件是否正在被写模式打开，违反了 Linux `execve(2)` 对 `ETXTBSY` 的要求。

## 修复

在 `OSFile` 层维护写打开计数：

- `OSFile::new()` 创建 writable 普通文件对象时，以 inode path 为 key 增加全局写打开计数；
- `OSFile::drop()` 在最后一个 `Arc<OSFile>` 释放时递减计数；
- fork/dup 只复制同一个 `Arc<OSFile>`，不会重复增加计数，符合 open file description 生命周期；
- `sys_execve()` 在读取目标 ELF 前调用 `OSFile::is_write_open_path()`，若目标 inode path 正被写打开，则返回 `ETXTBSY`；
- shebang 解释器 ELF 也走同样检查。

涉及文件：

- `os/src/fs/files/os_file.rs`
- `os/src/syscall/task/execve.rs`

## 验证

已执行：

```text
rustfmt os/src/fs/files/os_file.rs os/src/syscall/task/execve.rs
make
timeout 120s make run > log.ans 2>&1
```

当前默认 `TARGET_ARCH=loongarch64`，`make` 通过。

复现配置下单跑 musl/glibc `execve04`，两者均通过：

```text
execve04.c:52: TPASS: execve failed as expected: ETXTBSY (26)

Summary:
passed   1
failed   0
broken   0
skipped  0
warnings 0
```

`log.ans` 中未再出现 `execve_child shouldn't be executed`。
