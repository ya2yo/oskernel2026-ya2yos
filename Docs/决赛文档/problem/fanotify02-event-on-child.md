# fanotify02 目录子项事件与 remove mask 语义

## 背景

LTP `fanotify02` 验证目录 mark 的 child 事件语义。测试会在临时目录 `"."` 上添加：

```text
FAN_ACCESS | FAN_MODIFY | FAN_CLOSE | FAN_OPEN | FAN_EVENT_ON_CHILD | FAN_ONDIR
```

随后对该目录下的 `fname_$pid` 执行 open/write/close/read/close，并读取 fanotify fd 中的事件；之后再用 `FAN_MARK_REMOVE` 移除 `FAN_EVENT_ON_CHILD`，确认子文件事件不再产生，但目录自身 open/close 事件仍然保留。

## 现象

原始 `log.ans` 尾部卡在 `fanotify02` 的第一次事件读取：

```text
FanotifyInit ret = 3
FanotifyMark ret = 0
Openat ret = 4
Write ret = 7
Close ret = 0
Read sepc = ...
[block_current_and_run_next()] BEGIN!
QEMU: Terminated
```

`FanotifyFd::read()` 进入阻塞等待后没有事件入队，测试不再推进。

第一次修复 child 匹配后，测例继续推进到 remove 阶段，但又出现 `TBROK`：

```text
fanotify02.c:99: TBROK: fanotify_mark(3, 0x2, 0x8000000, ..., .) failed: EINVAL (22)
```

这里 `0x2` 是 `FAN_MARK_REMOVE`，`0x8000000` 是 `FAN_EVENT_ON_CHILD`。

## 分析

`fanotify01` 已经接入了基础 mark 表、事件队列和 open/read/write/close hook，但 mark 匹配仍只支持路径精确相等：

```text
marked_path == path
```

`fanotify02` 把 mark 加在临时目录 `"."` 上，实际触发事件的路径是该目录下的子文件。因此 `open/write/close` 虽然调用了 `notify_path_event()`，但 `push_if_marked()` 找不到匹配 mark，事件队列为空，`read(fd_notify)` 阻塞。

修复后暴露第二个语义问题：当前 `validate_fanotify_mark_mask()` 把只包含 `FAN_EVENT_ON_CHILD/FAN_ONDIR` 的 mask 一律视为非法。这个限制适用于 `FAN_MARK_ADD`，因为这些位只是事件修饰位，不能单独作为新增事件；但 `FAN_MARK_REMOVE` 需要允许单独移除这些位，否则无法按 Linux 语义清掉 child 事件订阅。

## 根因

根因有两个：

1. `FanotifyFd::push_if_marked()` 没有实现 `FAN_EVENT_ON_CHILD`，目录 mark 不会匹配直接子项路径，导致事件队列为空并引发阻塞。
2. `fanotify_mark()` 的 mask 校验没有区分 `ADD` 和 `REMOVE`，错误拒绝了 `FAN_MARK_REMOVE | FAN_EVENT_ON_CHILD`。

## 修复

涉及文件：

- `os/src/fs/files/fanotify.rs`
- `os/src/syscall/fs/fanotify.rs`

主要改动：

- 在 `FanotifyFd` 中新增 `FAN_EVENT_ON_CHILD` 常量和 `path_matches_mark()`；
- 精确路径仍按原 inode mark 语义匹配；
- 当 mark mask 含 `FAN_EVENT_ON_CHILD` 时，允许目录 mark 匹配直接子项路径，并用路径分隔符边界避免 `/tmp/foo` 误匹配 `/tmp/foobar`；
- `validate_fanotify_mark_mask()` 只在 `FAN_MARK_ADD` 且非 ignore mask 时拒绝纯 `FAN_EVENT_ON_CHILD/FAN_ONDIR`，`FAN_MARK_REMOVE` 允许单独移除这些修饰位。

当前实现仍是 fanotify 的最小兼容子集：只覆盖 LTP 当前需要的直接子项事件，不完整实现 recursive、mount/filesystem 传播和权限事件响应。

## 验证

已执行：

```text
cargo fmt --manifest-path os/Cargo.toml
make
/bin/bash -lc 'timeout 120s make run > /tmp/fanotify02-after-fix.log 2>&1'
```

当前默认架构为 LoongArch64，`make` 通过。

`/tmp/fanotify02-after-fix.log` 中 musl `fanotify02`：

```text
fanotify02.c:159: TPASS: got event: mask=20 pid=3 fd=4
fanotify02.c:159: TPASS: got event: mask=2 pid=3 fd=5
fanotify02.c:159: TPASS: got event: mask=8 pid=3 fd=6
fanotify02.c:159: TPASS: got event: mask=20 pid=3 fd=7
fanotify02.c:159: TPASS: got event: mask=1 pid=3 fd=8
fanotify02.c:159: TPASS: got event: mask=10 pid=3 fd=9
fanotify02.c:159: TPASS: got event: mask=20 pid=3 fd=10
fanotify02.c:159: TPASS: got event: mask=10 pid=3 fd=11
Summary:
passed   8
failed   0
broken   0
skipped  0
warnings 0
```

glibc `fanotify02` 同样 8 项 `TPASS`，summary 为：

```text
passed   8
failed   0
broken   0
skipped  0
warnings 0
```

日志中的包装行仍打印 `FAIL LTP CASE fanotify02 : 10` / `RESULT GLIBC LTP SINGLE CASE fanotify02 : 10`，但 LTP 内部 summary 与全部 `TPASS` 表明核心断言已经通过；按项目规则以 `TPASS/TFAIL/TBROK/Summary` 为准。
