# cyclictest musl 调度接口 stub 兼容修复

## 背景

当前 LoongArch 入口运行 `musl/cyclictest_testcode.sh`，脚本会执行 `NO_STRESS_P1`、`NO_STRESS_P8`、`STRESS_P1`、`STRESS_P8` 四个 cyclictest 场景。

cyclictest 启动时会检查当前调度策略和调度参数。如果调度接口不可用，会打印错误并退出。

## 现象

四个 cyclictest 子项均失败，用户态输出：

```text
unable to get scheduler parameters
====== cyclictest NO_STRESS_P1 end: fail ======
...
====== cyclictest STRESS_P8 end: fail ======
```

带 debug 日志运行时，失败前只能看到多次 `SchedGetaffinity ret = 8`，没有 `SchedGetParam` syscall 进入内核。

## 分析

最初怀疑 `sched_getaffinity` 返回 8 被用户态当作失败。但实验将返回值改为 0 后，cyclictest 退回到 libnuma CPU mask 推断失败：

```text
request to allocate mask for invalid number: Invalid argument
```

因此 `sched_getaffinity` 裸 syscall 返回 mask 字节数是当前 musl/libnuma 所需语义，不能改成 0。

继续从 ext4 镜像中提取 `/musl/cyclictest` 与 `/musl/lib/libc.so` 后反汇编确认：

- `cyclictest` 的 `check_privs()` 会调用 `sched_getscheduler(0)`。
- 若返回策略不是 `SCHED_FIFO/SCHED_RR`，会继续调用 `sched_getparam(0, &old_param)`。
- LoongArch musl `libc.so` 中 `sched_getparam`、`sched_getscheduler`、`sched_setparam`、`sched_setscheduler` 都是直接返回 `-ENOSYS` 的 stub，并未执行 syscall 指令。

所以内核的 `sys_sched_getparam()` 即使实现正确，也不会被该 musl libc 调用。

## 根因

测试镜像内 LoongArch musl libc 的部分 scheduler libc wrapper 是 `ENOSYS` stub，导致 cyclictest 在用户态 `sched_getparam()` 分支直接失败，内核 syscall 实现无机会执行。

## 修复

内核在读取动态链接文件 `/musl/lib/libc.so` 时，对返回给用户态的字节做 LoongArch 专用兼容补丁，不修改磁盘镜像：

- `sched_getparam` stub 改为写回 `sched_priority = 0` 并返回 0。
- `sched_getscheduler` / `sched_setparam` / `sched_setscheduler` stub 改为返回 0。
- `sys_sched_getparam` 同时补齐真实 syscall 语义：校验目标进程和用户指针，写回 4 字节 `sched_priority = 0`。
- 用户态测试库的 affinity wrapper 补齐 `cpusetsize = sizeof(usize)`，避免自测 wrapper 传 0。

涉及文件：

| 文件 | 修改 |
|------|------|
| `os/src/fs/map_dynamic_link.rs` | 新增 `/musl/lib/libc.so` 字节级 scheduler stub 补丁 |
| `os/src/fs/ext4_lw/inode.rs` | `read_at` / `read_all` 返回动态库内容前应用补丁 |
| `os/src/fs/mod.rs` | 导出动态库补丁 helper |
| `os/src/syscall/task/schedule.rs` | 补齐 `sched_getparam` 写回 `sched_priority = 0` |
| `os/src/syscall/mod.rs` | `sched_getparam` 参数按可写指针传入 |
| `user/src/syscall/mod.rs` | affinity wrapper 传入正确 `cpusetsize` |

## 验证

已执行：

```text
make
timeout 120s make run > /tmp/cyclictest-sched-fix.log 2>&1
```

结果：四个子项均通过，日志未再出现 `unable to get scheduler parameters` 或 `request to allocate mask`：

```text
====== cyclictest NO_STRESS_P1 end: success ======
====== cyclictest NO_STRESS_P8 end: success ======
====== cyclictest STRESS_P1 end: success ======
====== cyclictest STRESS_P8 end: success ======
#### OS COMP TEST GROUP END cyclictest-musl ####
shutdown!
```
