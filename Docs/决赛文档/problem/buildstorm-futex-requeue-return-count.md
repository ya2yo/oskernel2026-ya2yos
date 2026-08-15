# BuildStorm futex requeue 返回值过报导致死锁

## 背景

提交 `cadc845b778ff646a74c2c5a9ee3dc98be629456` 修复了 SMP 下 futex waiter
在 wake/requeue 期间丢失的问题，并改写了 `FUTEX_REQUEUE` 的队列处理。修复后
LoongArch64 BuildStorm 运行到 `arceos-helloworld` 构建阶段时，`log.ans` 长时间不再增长。

## 现象

挂起日志在 `OS COMP TEST GROUP START buildstorm` 之后持续运行约 2189 秒，最后一个阶段是：

```text
Running `target/debug/tg-xtask arceos build -p arceos-helloworld --arch loongarch64`
```

最终性能快照显示 `ready_tasks=0`、所有可运行 hart 进入 idle，而不是测试主动退出；futex
调用计数在此前仍持续增长，符合用户态条件变量 waiter 计数失真的死锁特征。

## 分析

Linux `FUTEX_REQUEUE` 的返回值是本次从旧 futex **直接唤醒**的 waiter 数量。被移动到新
futex 的 waiter 仍处于阻塞状态，不能计入返回值。pthread 条件变量实现会使用该返回值更新
用户态 waiter/信号计数。

提交中的 `futex_requeue()` 在完成 wake/requeue 后返回 `woken + requeued`。当大量 waiter
被重排时，用户态收到的成功唤醒数大于实际已 Ready 的线程数，条件变量内部计数与内核队列
分离；随后可能没有线程再发出对应 wake，最终所有任务阻塞，表现为 `log.ans` 停止增长。

## 根因

`FUTEX_REQUEUE` 返回值错误地包含了 requeue 数量，违反 Linux futex ABI。

## 修复

`futex_requeue()` 仅返回 `woken`。重排 waiter 仍保留在新队列并继续由其后续 wake/timeout
完成；提交中对 generation 检查、超时竞争和未移动 waiter 保留逻辑不变。

## 涉及文件

- `os/src/task/futex.rs`

## 验证

- `git diff --check` 通过。
- `cargo fmt --manifest-path os/Cargo.toml -- --check` 通过。
- `make build-arch TARGET_ARCH=riscv64` 通过。
- `make build-arch TARGET_ARCH=loongarch64` 通过，仅有既有依赖未使用项警告。
- 本轮未重新执行完整 QEMU/BuildStorm：该测试需要长时间宿主 QEMU 运行环境；因此不宣称
  端到端死锁回归已完成。
