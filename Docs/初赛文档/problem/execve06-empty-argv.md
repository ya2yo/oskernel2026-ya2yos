# execve06: 空 argv 未补充 argv[0]

## 背景

LTP `execve06` 验证 Linux `execve(2)` 对空参数表的兼容行为。测试调用：

```text
char *const argv[] = { NULL };
execve(path, argv, envp);
```

新内核应在参数表为空时补一个 dummy `argv[0]`，使新程序至少看到 `argc == 1` 且 `argv[0] != NULL`。该行为对应 Linux 修复 `CVE-2021-4034` 的提交 `dcd46d897adb`。

## 现象

修复前新的 `log.ans` 中，musl `execve06` 子进程因空 argv 路径触发用户态空地址访问：

```text
Exception(LoadPageFault) in application, bad addr = 0x0, sending SIGSEGV.
tst_test.c:391: TBROK: Child (4) killed by signal SIGSEGV
```

glibc 路径能进入子程序，但看到 `argc == 0`：

```text
execve06_child.c:15: TFAIL: argc is 0, expected 1
```

## 分析

`execve06.c` 显式传入 `argv = { NULL }`。Ya2yOS 原 `sys_execve()` 的 argv 解析逻辑为：

- 读取 `argv[0]`；
- 若 `argv[0] != NULL`，才向 `argv_vec` 推入路径并继续读取后续参数；
- 若 `argv[0] == NULL`，循环立刻结束，`argv_vec` 保持为空。

后续 `TaskControlBlock::exec()` 按 `argv.len()` 写入用户栈，因此新程序得到 `argc == 0`。musl LTP 框架在该路径上还会进一步访问空 argv，导致 SIGSEGV。

Linux 兼容语义要求：即使用户传入空参数表，内核也要补一个非空的 `argv[0]` 指针。测试只要求 `argc == 1` 且 `argv[0] != NULL`，不要求字符串内容。

## 根因

`sys_execve()` 没有处理空 argv 表，直接把空 `argv_vec` 传给新程序，导致 `argc=0`，违反 Linux 对空 argv 的安全兼容行为。

## 修复

在 `sys_execve()` 完成 argv 解析后增加空表兜底：

```text
if argv_vec.is_empty() {
    argv_vec.push(String::new());
}
```

这样用户栈会写入一个只含 NUL 的字符串，`argc` 为 1，`argv[0]` 指向该字符串地址，满足 Linux 兼容行为和 LTP 断言。

涉及文件：

- `os/src/syscall/task/execve.rs`

## 验证

已执行：

```text
rustfmt os/src/syscall/task/execve.rs
make
timeout 120s make run > log.ans 2>&1
```

当前默认 `TARGET_ARCH=loongarch64`，`make` 通过。

复现配置下单跑 musl/glibc `execve06`，两者均通过：

```text
execve06_child.c:24: TPASS: argv[0] was filled in by kernel

Summary:
passed   1
failed   0
broken   0
skipped  0
warnings 0
```

`log.ans` 中未再出现 `argc is 0, expected 1` 或 `Child killed by signal SIGSEGV`。
