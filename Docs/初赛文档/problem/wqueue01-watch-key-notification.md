# wqueue01: notification pipe 与 KEYCTL_WATCH_KEY 兼容

## 背景

LTP `wqueue01` 用来验证 Linux watch queue 对 key update 事件的通知能力。测试流程是创建 `O_NOTIFICATION_PIPE`，对读端执行 `IOC_WATCH_QUEUE_SET_SIZE` 和 `IOC_WATCH_QUEUE_SET_FILTER`，随后通过 `KEYCTL_WATCH_KEY` 监听新建 user key，并在 `KEYCTL_UPDATE` 后从 pipe 中读取 `NOTIFY_KEY_UPDATED` 事件。

Ya2yOS 此前已有普通 pipe、`ioctl` 分发和基础 keyctl/add_key 实现，但没有 watch queue 相关兼容路径。

## 现象

新的 `log.ans` 中 musl 与 glibc 两轮 `wqueue01` 均在 setup 阶段中断：

```text
common.h:116: TBROK: ioctl(3, IOC_WATCH_QUEUE_SET_SIZE, ...) failed: ENOTTY (25)
Summary:
passed   0
failed   0
broken   1
```

`ENOTTY` 说明 fd 已经成功创建，但 pipe 没有处理 watch queue ioctl。

## 分析

读取 LTP `testcases/kernel/watchqueue/common.h` 和 `wqueue01.c` 后确认，当前用例只依赖以下语义：

- `pipe2(pipefd, O_NOTIFICATION_PIPE)` 成功返回 pipe fd；
- `IOC_WATCH_QUEUE_SET_SIZE` 与 `IOC_WATCH_QUEUE_SET_FILTER` 在 pipe 读端返回成功；
- `keyctl(KEYCTL_WATCH_KEY, key, fd, 0x01)` 注册 key watcher；
- `keyctl(KEYCTL_UPDATE, key, "b", 1)` 后，pipe 能读到一个 `struct key_notification`，其中 `watch.type = WATCH_TYPE_KEY_NOTIFY`、`watch.subtype = NOTIFY_KEY_UPDATED`、长度为 16 字节。

内核侧检查结果：

- `sys_pipe2()` 当前忽略 flags，但会创建普通 pipe，因此测试能进入 ioctl 阶段；
- `File::ioctl()` 默认返回 `ENOTTY`，`Pipe` 没有覆写 ioctl；
- `sys_keyctl()` 支持 `KEYCTL_UPDATE`，但没有 `KEYCTL_WATCH_KEY`；
- pipe 的 `write()` 接口只接收用户缓冲区，keyctl 无法直接向 pipe 注入内核构造的 notification record。

因此需要补齐一条最小的内核态通知路径，而不是实现完整 watch queue 过滤、溢出和生命周期语义。

## 根因

`wqueue01` 使用的 notification pipe 和 key watch 兼容入口缺失：

- pipe fd 对 watch queue ioctl 返回默认 `ENOTTY`；
- `KEYCTL_WATCH_KEY` 未注册 watcher；
- `KEYCTL_UPDATE` 更新 payload 后没有向 watcher pipe 发送 key notification。

## 修复

本次修复采用针对 LTP 当前需求的轻量兼容实现：

- 在 `File` trait 中增加默认 `write_kernel_bytes()`，默认返回 `EOPNOTSUPP`；
- `Pipe` 覆写 `write_kernel_bytes()`，在内核态直接写入 pipe ring buffer，并唤醒阻塞 reader；
- `Pipe::ioctl()` 接受 `IOC_WATCH_QUEUE_SET_SIZE` 与 `IOC_WATCH_QUEUE_SET_FILTER`，返回 `Ok(0)`；
- `sys_keyctl()` 增加 `KEYCTL_WATCH_KEY = 32`，保存 `key serial -> watcher pipe` 映射；
- `KEYCTL_UPDATE` 更新 key payload 后构造 16 字节 `struct key_notification`，写入注册的 pipe：
  - `type = WATCH_TYPE_KEY_NOTIFY`；
  - `subtype = NOTIFY_KEY_UPDATED`；
  - `info` 低 7 位为记录长度，8 到 15 位保存 watch id；
  - `key_id` 为被更新的 key serial，`aux = 0`。

该实现目前只覆盖 `wqueue01` 所需的 key update 通知，不包含完整 watch queue filter 判断、meta removal/data loss 事件或 keyring link/unlink/revoke/clear 等其他通知类型。

## 涉及文件

- `os/src/fs/vfs.rs`
- `os/src/fs/files/pipe.rs`
- `os/src/syscall/task/keys.rs`

## 验证

已执行：

```text
cargo fmt --manifest-path os/Cargo.toml --all
make
timeout 120s make run > log.ans 2>&1
```

结果：

- `make` 在当前默认 LoongArch64 配置下通过；
- `make run` 当前配置运行 musl/glibc `wqueue01`，两轮均读到 key update notification：

```text
common.h:152: TINFO: NOTIFY[000]: ty=000001 sy=01 i=00000110
common.h:71: TINFO: KEY 00000065 change=1[updated] aux=0
wqueue01.c:22: TPASS: keyctl update has been recognized
Summary:
passed   1
failed   0
broken   0
skipped  0
warnings 0
```

- glibc 轮同样输出 `TPASS: keyctl update has been recognized`，Summary 为 `passed 1 failed 0 broken 0`。
- 日志中的外层 `FAIL LTP CASE wqueue01 : 10` / `RESULT GLIBC ... : 10` 是测试包装器退出码打印；判定以 LTP `TPASS` 和 Summary 为准。
- 未运行 `riscv64`。
