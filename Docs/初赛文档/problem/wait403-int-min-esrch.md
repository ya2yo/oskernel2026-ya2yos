# wait403: wait4(INT_MIN) errno

## 背景

LTP `wait403` 验证 `wait4()` 对非法 `pid` 参数的 errno。`waitpid/wait4` 语义中，`pid < -1` 表示等待进程组 ID 为 `-pid` 的子进程。

## 现象

`log.ans` 中 `wait403` 失败：

```text
wait403.c:31: TFAIL: wait4 fails with ESRCH expected ESRCH: ECHILD (10)
```

也就是测例期望 `ESRCH`，但内核返回了 `ECHILD`。

## 分析

`sys_waitpid()` 原逻辑把所有 `pid <= -2` 都归类为 `WaitPid::Pgid((-pid) as u32)`。当传入 `i32::MIN` 时，`-pid` 在 `i32` 范围内无法表示，release 构建下会绕回原值，再被转换成一个假的 pgid。

随后当前内核尚未实现完整进程组匹配，`WaitPid::Pgid` 不匹配任何子进程，最终统一返回 `ECHILD`。这掩盖了参数本身非法的问题。

## 根因

`sys_waitpid()` 缺少 `pid == i32::MIN` 的特殊处理，导致不可取反的非法 pid 被当成普通进程组等待请求处理。

## 修复

在解析 `wait_pid` 前增加显式检查：

```rust
if pid == i32::MIN {
    return Err(SysErrNo::ESRCH);
}
```

这样非法 selector 直接返回 Linux 期望的 `ESRCH`，普通 `pid < -1` 的进程组等待路径保持不变。

## 涉及文件

- `os/src/syscall/task/wait.rs`

## 验证

已执行：

```text
make
timeout 90s make run
```

结果：

```text
wait403.c:31: TPASS: wait4 fails with ESRCH : ESRCH (3)

Summary:
passed   1
failed   0
broken   0
skipped  0
warnings 0
```
