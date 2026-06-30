# getrlimit03: prlimit64 与 getrlimit 返回不一致

## 背景

LTP `getrlimit03` 用来比较不同 rlimit syscall 的返回结果是否一致。测试会对 `0..RLIM_NLIMITS` 的每个 resource 分别调用：

- `prlimit64(pid=0, resource, NULL, &rlim_u64)`
- `getrlimit(resource, &rlim_ul)`

然后比较返回值、errno、`rlim_cur` 和 `rlim_max`。在 64 位 LoongArch musl 环境下，`getrlimit()` 和 `prlimit64()` 都应该返回 64 位 `rlim_t` 兼容结果。

## 现象

新的 `log.ans` 中只运行了 `ltp-musl/getrlimit03`，失败集中在 `prlimit64()` 与 `getrlimit()` 返回的 soft limit 不一致：

```text
getrlimit03.c:117: TFAIL: __NR_prlimit64(0) had rlim_cur = 5 but __NR_getrlimit(0) had rlim_cur = ffffffffffffffff
getrlimit03.c:117: TFAIL: __NR_prlimit64(1) had rlim_cur = 5 but __NR_getrlimit(1) had rlim_cur = ffffffffffffffff
getrlimit03.c:117: TFAIL: __NR_prlimit64(3) had rlim_cur = 5 but __NR_getrlimit(3) had rlim_cur = 800000
getrlimit03.c:117: TFAIL: __NR_prlimit64(8) had rlim_cur = 80 but __NR_getrlimit(8) had rlim_cur = ffffffffffffffff
```

其中 resource 7 通过：

```text
getrlimit03.c:174: TPASS: __NR_prlimit64(7) and __NR_getrlimit(7) gave consistent results
```

最终汇总：

```text
Summary:
passed   1
failed   15
broken   0
skipped  0
warnings 0
FAIL LTP CASE getrlimit03 : 10
```

## 分析

内核中 `getrlimit()` 与 `prlimit64()` 分别位于两个文件：

- `os/src/syscall/task/resource.rs`
- `os/src/syscall/resource.rs`

`sys_getrlimit()` 对 `RLIMIT_NOFILE` 返回 fd table 当前限制，对 `RLIMIT_STACK` 返回默认 8MiB，其它 resource 返回 `RLIM_INFINITY`。

但 `sys_prlimit()` 原先只处理 `RLIMIT_NOFILE`：

```rust
const RLIMIT_NOFILE: u32 = 7;
if resource != RLIMIT_NOFILE {
    return Ok(0);
}
```

这意味着非 `RLIMIT_NOFILE` 的 `prlimit64(pid=0, old_limit != NULL)` 会直接返回成功，却没有向用户态 `old_limit` 写入任何数据。LTP 用户态结构体中的残留栈值就表现为 `rlim_cur = 5` 或 `80`，与 `getrlimit()` 的默认值不一致。

resource 7 通过，是因为 `RLIMIT_NOFILE` 正好走了 fd table 分支，会正确写回 `old_limit`。

## 根因

`sys_prlimit()` 对非 `RLIMIT_NOFILE` resource 直接返回 `Ok(0)`，遗漏了 `old_limit` 写回；同时 `prlimit64()` 和 `getrlimit()` 没有共用同一套默认 rlimit 语义。

## 修复

修改 `os/src/syscall/task/resource.rs`：

- 将 `RLIMIT_NOFILE`、`RLIMIT_STACK`、`RLIM_INFINITY` 和 `default_rlimit()` 暴露给 `prlimit64()` 复用。

修改 `os/src/syscall/resource.rs`：

- `pid != 0` 时返回 `ESRCH`，避免原先 `unimplemented!()` panic。
- `old_limit` 非空时：
  - `RLIMIT_NOFILE` 返回 fd table 当前 soft/hard limit。
  - 其它 resource 返回 `default_rlimit(resource)`，与 `getrlimit()` 保持一致。
- `new_limit` 非空时读取用户结构并校验 `rlim_cur <= rlim_max`。
- 当前仍只对 `RLIMIT_NOFILE` 实际更新 fd table 限制，其它 resource 沿用原 `setrlimit()` 的静默接受策略。

## 涉及文件

- `os/src/syscall/resource.rs`
- `os/src/syscall/task/resource.rs`

## 验证

已执行：

```text
make
```

结果：

- 当前默认 `TARGET_ARCH=loongarch64`，构建通过。
- AI 环境中 `make run` 因 QEMU 需要写 `/var/tmp` 而被沙箱阻止，未能在本地完成运行验证。
- 维护者随后确认单跑验证通过。

未执行：

- 未运行 `riscv64`。
