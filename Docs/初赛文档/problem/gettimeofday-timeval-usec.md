# gettimeofday: timeval 微秒字段写回错误

## 背景

用户态 `gettimeofday(2)` 返回 `struct timeval`，第二个字段是微秒 `tv_usec`，取值应位于 `[0, 1000000)`。`getrusage(2)` 的 `ru_utime` / `ru_stime` 也使用 `struct timeval` 表示 CPU 时间。

## 现象

测试代码在调用 `GetTimeOfDay` 和 `GetRusage` 后，用如下逻辑做 timeval 差值：

```c
tdiff->tv_usec = t1->tv_usec - t0->tv_usec;
if (tdiff->tv_usec < 0 && tdiff->tv_sec > 0) {
    tdiff->tv_sec--;
    tdiff->tv_usec += 1000000;
    assert(tdiff->tv_usec >= 0);
}
```

断言失败说明某个 `timeval.tv_usec` 不满足微秒规范，负差值小于 `-1000000`，补一次借位后仍为负数。

## 分析

`Rusage::new_from_ms_with_maxrss()` 使用 `TimeVal::new(sec, usec)` 构造 CPU 时间，`usec = (ms % 1000) * 1000`，不会超过微秒上限。

问题出在 `sys_gettimeofday()`：旧实现的参数类型是 `*mut Timespec`，并把 `get_time_spec()` 的 `tv_nsec` 直接拷贝到用户缓冲。用户态 libc 以 `struct timeval` 解析同一块内存，因此 `tv_usec` 实际收到纳秒值，范围可到 `999000000`。当该值与 `getrusage` 返回的微秒字段混用时，`tvsub()` 的一次 `+1000000` 借位无法修正纳秒/微秒量纲差。

## 根因

`gettimeofday` syscall 出口混用了 `Timespec` 和 `TimeVal`：

- syscall 分发表把 `GetTimeOfDay` 参数转换为 `*mut Timespec`
- `sys_gettimeofday()` 写回 `Timespec { tv_sec, tv_nsec }`
- 用户态实际期望 `timeval { tv_sec, tv_usec }`

## 修复

涉及文件：

| 文件 | 修改 |
|------|------|
| `os/src/syscall/time.rs` | `sys_gettimeofday()` 参数改为 `*mut TimeVal`，使用 `TimeVal::now()` 写回微秒字段，并叠加 `NOW_TIME_STAMP` 与 `CLOCK_REALTIME_OFFSET` |
| `os/src/syscall/time.rs` | 允许 `tv == NULL` 时不写时间；`tz != NULL` 时写零 timezone，使坏 timezone 指针能通过 `copy_to_user` 返回 `EFAULT` |
| `os/src/syscall/mod.rs` | `GetTimeOfDay` 分发改为传入 `*mut TimeVal` |

## 验证

已执行：

```text
make
```

结果：LoongArch64 当前目标编译通过。

已执行：

```text
timeout 120s make run
```

结果：当前工作区配置运行 glibc lmbench，测试推进到 fork/shell/文件带宽阶段，未出现 `tvsub` 断言、panic 或时间相关失败；外层 120 秒 timeout 截断 QEMU，因此该结果只说明当前触发路径未再复现断言，不代表整套 lmbench 完整 PASS。
