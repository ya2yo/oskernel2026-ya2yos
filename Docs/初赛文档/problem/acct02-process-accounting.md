# acct02: process accounting 记录缺失与内核配置缺失

## 背景

LTP `acct02` 用于验证 `acct(2)` 进程记账功能。测例会先通过 `tst_kconfig` 检查 `CONFIG_BSD_PROCESS_ACCT`，再调用 `acct(OUTPUT_FILE)` 开启系统级 accounting，运行 `acct02_helper`，关闭 accounting 后读取输出文件并校验 `struct acct` 内容。

当前单跑配置为 LoongArch musl `acct02`。

## 现象

初始 `log.ans` 中 `acct02` 在进入主体校验前失败：

```text
tst_kconfig.c:71: TINFO: Couldn't locate kernel config!
tst_kconfig.c:207: TBROK: Cannot parse kernel .config
```

补齐 kconfig 后，测例继续暴露下一层问题：`acct()` 只保存了 accounting 文件句柄，进程退出时没有写出任何 accounting 记录，或记录中的退出状态被内部 SIGKILL 污染。

## 分析

LTP 会根据 `uname.release` 探测多个内核配置路径。当前 `sys_uname()` 返回 `5.0.0`，日志显示测例依次访问：

```text
/proc/config.gz
/lib/modules/5.0.0/build/.config
/lib/modules/5.0.0/config
/boot/config-5.0.0
/lib/kernel/config-5.0.0
```

这些路径均不存在，因此 `tst_kconfig` 无法确认 `CONFIG_BSD_PROCESS_ACCT`。

继续查看 `acct02` 源码后可知，测例会在 `CONFIG_BSD_PROCESS_ACCT_V3=n` 时按旧版 `struct acct` 读取记录，并校验：

- `ac_comm` 等于 `acct02_helper`
- `ac_btime` 接近当前 `time(NULL)`
- `ac_uid` / `ac_gid` 等于当前进程 uid/gid
- `ac_utime` / `ac_stime` / `ac_etime` 在合理范围内
- `ac_exitcode` 等于 `tst_cmd()` 观察到的返回状态

原实现的 `sys_acct()` 仅在全局变量里保存目标文件，退出路径没有写记录。同时，`exit_group()` 正常退出时内部调用 `send_signal_to_thread_group(pid, SIGKILL)` 唤醒同线程组其他线程，这个内部 SIGKILL 会被错误写入 `ProcessMeta::termination_signal`，导致 accounting 记录把正常退出误记成信号 9 终止。

## 根因

1. 内核没有提供 LTP `tst_kconfig` 可读取的配置文件，导致 `CONFIG_BSD_PROCESS_ACCT` 检查直接 `TBROK`。
2. `acct(2)` 没有在进程退出时写出 Linux 兼容的旧版 `struct acct` 记录。
3. 正常 `exit_group()` 的内部 SIGKILL 清理路径错误覆盖了真实终止原因，使 accounting 记录的 `ac_exitcode` 被污染。

## 修复

- 在初始化文件阶段创建 `/boot/config-5.0.0`，写入最小配置：
  - `CONFIG_BSD_PROCESS_ACCT=y`
  - `# CONFIG_BSD_PROCESS_ACCT_V3 is not set`
- 在进程元数据中维护 Linux `comm` 短命令名：
  - `exec()` 时从 `argv[0]` 提取 basename
  - `fork/clone` 创建新进程时继承父进程 `comm`
- 在 `acct.rs` 中实现旧版 `struct acct` 记录：
  - 显式建模 C ABI padding，保证记录大小为 64 字节
  - 写入 `uid/gid/btime/comm/exitcode` 等字段
  - 将 CPU 时间按 `comp_t` 格式编码
- 在最后一个线程退出、资源使用快照计算完成后，追加写 accounting 记录。
- `send_signal_to_thread_group()` 在进程已存在 group exit code 时，不再用后续内部信号覆盖 `termination_signal`。

涉及文件：

- `os/src/fs/kernel_fs_ops/initfiles.rs`
- `os/src/syscall/task/acct.rs`
- `os/src/task/mod.rs`
- `os/src/task/process/process.rs`
- `os/src/task/task/task.rs`
- `os/src/signal/mod.rs`

## 验证

已执行：

```text
make
make log
timeout 300s make run > log.ans 2>&1
```

当前 LoongArch musl 单跑 `acct02` 输出：

```text
acct02.c:63: TINFO: CONFIG_BSD_PROCESS_ACCT_V3=n
acct02.c:243: TINFO: Verifying using 'struct acct'
acct02.c:193: TINFO: == entry 1 ==
acct02.c:204: TINFO: Number of accounting file entries tested: 1
acct02.c:210: TPASS: acct() wrote correct file contents!
Summary:
passed 1
failed 0
broken 0
```

`log.ans` 尾部的 `FAIL LTP CASE acct02 : 0` 是 initproc 当前打印格式，退出码为 0；实际 LTP Summary 为通过。
