# alarm05: setitimer old_value 剩余时间语义修复

## 背景

LTP `alarm05` 校验 `alarm(2)` 与 `SIGALRM` 的基本语义。用户态 `alarm()` 通常通过 `setitimer(ITIMER_REAL, ...)` 实现，核心要求包括：

- 新闹钟能替换旧闹钟。
- `alarm()` 返回被替换旧闹钟的剩余秒数。
- 单次闹钟到期后只投递一次 `SIGALRM`。

Ya2yOS 的 `Timer` 已经记录 `it_value`、`it_interval` 和 `last_time`，能驱动 `ITIMER_REAL` 到期投递 `SIGALRM`，但此前读取 timer 状态时返回的是保存的原始 `it_value`。

## 现象

修复前 `log.ans` 中 `alarm05` 第二个断言失败：

```text
alarm05.c:28: TPASS: alarm(10) passed
alarm05.c:30: TFAIL: alarm(1) retval 10 != 9: SUCCESS (0)
alarm05.c:32: TPASS: alarms_fired == 1 (1)
```

日志时序显示子进程先设置 `alarm(10)`，睡眠约 1 秒后调用 `alarm(1)`。此时旧闹钟应还剩约 9 秒，但内核返回了原始设置值 10。

## 分析

`sys_settimer()` 在用户传入 `old_value` 时会先读取当前 timer，然后再安装新 timer。旧 `Timer::timer()` 只是直接返回内部保存的 `Itimerval`：

```rust
pub fn timer(&self) -> Itimerval {
    self.inner.get_unchecked_ref().timer
}
```

这对“是否能按时投递信号”影响不大，因为投递路径会用 `last_time + duration` 判断是否到期；但对 `getitimer()` 和 `setitimer(old_value)` 是错误的。Linux 语义要求返回当前剩余时间，而不是最初设置的持续时间。

因此 `alarm(10)` 后经过 1 秒，`alarm(1)` 仍从 `old_value.it_value` 读到 10 秒，LTP 判断失败。

同时原周期 timer 到期判断中，非空 `it_interval` 会直接使用 interval 作为比较 duration：

```rust
let duration = if inner.timer.it_interval.is_empty() {
    inner.timer.it_value
} else {
    inner.timer.it_interval
};
```

这会把首次到期时间也错误地按 `it_interval` 判断。标准语义应使用 `it_value` 作为当前这一次的剩余/到期时间，周期触发后再把下一轮 `it_value` 设为 `it_interval`。

## 根因

`Timer` 内部把 `it_value` 当成“设置时的相对持续时间”保存，但对外读取时没有用 `now - last_time` 折算成“当前剩余时间”。实现上混合了两种模型：

- 到期判断路径使用 `last_time` 推导是否过期。
- 查询/替换路径直接返回原始 `it_value`。

所以 timer delivery 看起来基本正常，但 `alarm()` 的返回值语义不兼容 Linux。

## 修复

修复集中在 timer 语义层：

- `TimeVal` 增加 `saturating_sub()`，用于安全计算经过时间和剩余时间，避免 timer 已过期时下溢。
- `Timer::timer(now)` 改为返回实时剩余 `it_value`：`remaining = it_value - (now - last_time)`，已过期则饱和为 0。
- `sys_gettimer()` 和 `sys_settimer(old_value)` 调用 `timer(TimeVal::now())`，确保用户态看到的是当前剩余时间。
- `sys_settimer()` 中 old timer 读取和新 timer 安装共用同一个 `now`，避免替换边界处前后基准时间不一致。
- 周期 timer 到期后将 `it_value` 更新为 `it_interval`，下一轮再按新的当前持续时间计算。

这次没有改 signal frame、trap timer interrupt 或任务调度逻辑，因为 `alarm05` 的失败点不是信号投递，而是旧闹钟剩余时间返回值。

## 涉及文件

| 文件 | 修改 |
|------|------|
| `os/src/timer/timeval.rs` | 增加 `USEC_PER_SEC` 常量和 `TimeVal::saturating_sub()` |
| `os/src/timer/itimerval.rs` | `Timer::timer()` 改为按当前时间返回剩余值；周期 timer 到期后推进到下一轮 interval |
| `os/src/syscall/time.rs` | `getitimer/setitimer(old_value)` 使用实时剩余 timer，并让替换操作共用同一 `now` |

## 验证

已执行：

```text
make
```

结果：默认 RISC-V 构建通过，仅有既有 warning。

AI 曾尝试在沙箱内运行 `make run`，但 QEMU 需要在只读 `/var/tmp` 创建临时文件，未能启动：

```text
qemu-system-riscv64: ... Could not open temporary file '/var/tmp/...': Read-only file system
```

随后用户在可运行环境中完成验证，最新 `log.ans` 显示 `alarm05` 核心断言全部通过：

```text
alarm05.c:28: TPASS: alarm(10) passed
alarm05.c:30: TPASS: alarm(1) passed
alarm05.c:32: TPASS: alarms_fired == 1 (1)

Summary:
passed   3
failed   0
broken   0
skipped  0
warnings 0
```

日志中仍有包装器行：

```text
FAIL LTP CASE alarm05 : 10
```

但 LTP 本体 summary 为 `passed 3 failed 0 broken 0`，本仓库判读 LTP 结果时以 `TPASS/TFAIL/TBROK/Summary` 为准。

未执行 `TARGET_ARCH=loongarch64` 验证；本次复现与验证基于当前默认 RISC-V 配置和用户提供的最新 `log.ans`。
