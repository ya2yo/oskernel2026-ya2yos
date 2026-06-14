# wait402: /proc/sys/kernel/pid_max 缺失

## 背景

LTP `wait402` 会先读取 `/proc/sys/kernel/pid_max`，再调用 `wait4(pid_max + 1, &status, 0, &rusage)` 验证非法子进程号场景应返回 `ECHILD`。

## 现象

`log.ans` 中 `wait402` 失败在读取 proc sysctl 文件阶段：

```text
wait402.c:30: TBROK: Failed to open FILE '/proc/sys/kernel/pid_max' for reading: ENOENT (2)
```

对应内核日志显示路径解析已经得到绝对路径：

```text
[sys_openat] path is /proc/sys/kernel/pid_max, flags is O_LARGEFILE, mode is 666
open(/proc/sys/kernel/pid_max,O_LARGEFILE,438)
abs_path is /proc/sys/kernel/pid_max
```

## 分析

`create_init_files()` 已经创建了 `/proc/sys`、`/proc/sys/kernel` 和 `/proc/sys/kernel/tainted`，说明父目录存在，`open()` 返回 `ENOENT` 的原因不是路径归一化或挂载点缺失。

Ya2yOS 当前 PID 分配由 `IdAllocator` 递增管理，没有对外暴露 sysctl 文件。`wait402` 只需要读取一个合法整数，用来构造超过最大 PID 的 `wait4()` 参数；该测例不依赖动态修改 `pid_max`。

## 根因

proc 初始化只补了 `/proc/sys/kernel/tainted`，没有创建 Linux 常见的 `/proc/sys/kernel/pid_max` 节点，导致 LTP 在真正测试 `wait4()` 前被 `TBROK` 中断。

## 修复

在 `os/src/fs/kernel_fs_ops/initfiles.rs` 中新增静态 proc sysctl 文件：

- 增加 `PID_MAX = "4194304\n"`
- 启动初始化时创建 `/proc/sys/kernel/pid_max`
- 写入 Linux 常见 PID 上限值，供 LTP 读取

该修复只补齐兼容性节点，不改变内核现有 PID 分配逻辑。

## 涉及文件

- `os/src/fs/kernel_fs_ops/initfiles.rs`

## 验证

已执行：

```text
make
timeout 90s make run
```

结果：

```text
wait402.c:25: TPASS: wait4(pid_max + 1, &status, 0, &rusage) : ECHILD (10)

Summary:
passed   1
failed   0
broken   0
skipped  0
warnings 0
```

不再出现 `/proc/sys/kernel/pid_max` 的 `ENOENT` / `TBROK`。
