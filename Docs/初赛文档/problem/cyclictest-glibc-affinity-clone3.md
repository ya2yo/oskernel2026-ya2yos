# cyclictest glibc 失败修复

## 背景

LoongArch 当前入口运行 `glibc/cyclictest_testcode.sh`，脚本依次执行 `NO_STRESS_P1`、`NO_STRESS_P8`、`STRESS_P1`、`STRESS_P8` 四个 cyclictest 场景。cyclictest 会创建实时线程、设置 CPU affinity、锁定内存，并用定时睡眠统计唤醒延迟。

## 现象

最初 `log.ans` 中 `NO_STRESS_P1` 在启动阶段打印：

```text
request to allocate mask for invalid number: Invalid argument
====== cyclictest NO_STRESS_P1 end: fail ======
```

修复 affinity 后继续推进，又出现：

```text
ERROR, mlock Bad address
FATAL: failed to create thread 0: Invalid argument
```

## 分析

cyclictest/libnuma 会先通过 `/sys/devices/system/cpu/possible`、`/proc/stat` 和 `sched_getaffinity()` 推断可用 CPU 掩码。当前内核没有 sysfs/proc CPU 节点，因此 `sched_getaffinity()` 的返回值和写回 mask 是关键 fallback。

原实现直接 `Ok(0)`，既没有写用户态 mask，也没有返回 mask 字节数。glibc/libnuma 将其解释为无效 CPU mask，触发 “request to allocate mask for invalid number”。

修复 affinity 后，cyclictest 继续执行到内存锁定和线程创建。`mlock` 当前是 no-op，但实现仍要求地址页对齐且整段已映射，cyclictest 传入跨页/边界范围时返回 `EFAULT`。随后 glibc pthread 使用 `clone3` 创建线程，参数中 `pidfd`、`parent_tid`、`child_tid` 指向同一用户地址，但 flags 未包含 `CLONE_PIDFD`；Linux 语义下未设置 `CLONE_PIDFD` 时应忽略 `pidfd` 字段，原实现误判为 `EINVAL`。

## 根因

1. `sys_sched_getaffinity()` 不写 CPU mask，且成功返回 0，不符合 Linux 成功返回写入字节数的语义。
2. `mlock` 作为 no-op syscall 仍做了过强的映射区间校验，导致 cyclictest 的内存锁定探测失败。
3. `clone3` 在未设置 `CLONE_PIDFD` 时仍校验 `pidfd` 字段，拒绝 glibc pthread 的常见参数组合。

## 修复

- `sys_sched_getaffinity()`：校验 `cpusetsize` 和用户指针，写回单核 CPU0 mask `1usize`，返回 `sizeof(usize)`。
- `sys_sched_setaffinity()`：校验用户 mask 指针与目标 PID/TID 存在；当前单核调度不实际迁核，接受用户传入的 affinity mask。
- `mlock`/`munlock`/`mlock2`：保留长度溢出与 bad-address 检查，移除页对齐和整段已映射要求，使 no-op 行为与当前无 swap 内核一致。
- `clone3`：未设置 `CLONE_PIDFD` 时不再因为 `pidfd` 字段非零返回 `EINVAL`；真正请求 `CLONE_PIDFD` 时仍保持不支持返回。

涉及文件：

- `os/src/syscall/task/schedule.rs`
- `os/src/syscall/mm/mlock.rs`
- `os/src/syscall/task/clone3.rs`

## 验证

已执行：

```text
make
timeout 120s make run > log.ans 2>&1
```

`log.ans` 中四个 cyclictest 子项均通过：

```text
====== cyclictest NO_STRESS_P1 end: success ======
====== cyclictest NO_STRESS_P8 end: success ======
====== cyclictest STRESS_P1 end: success ======
====== cyclictest STRESS_P8 end: success ======
```

未再出现 `request to allocate mask`、`ERROR, mlock Bad address`、`FATAL: failed to create thread 0` 或 `Clone3 ret = Invalid argument`。
