# acct02: accounting exit(128) 状态编码修复

## 背景

LTP `acct02` 会开启 BSD process accounting，运行 `acct02_helper`，关闭 accounting 后读取旧版 64 字节 `struct acct` 记录，并校验 `ac_comm`、时间、uid/gid 和 `ac_exitcode` 等字段。

此前 Ya2yOS 已实现基础 accounting 记录写出，本次回归出现在普通进程退出码的 wait status 编码上。

## 现象

本轮 `log.ans` 中 `acct02` 读取到 1 条 accounting 记录，但退出状态字段不符合预期：

```text
acct02.c:193: TINFO: == entry 1 ==
acct02.c:133: TINFO: ac_exitcode != 32768 (0)
acct02.c:183: TFAIL: end of file reached
```

日志中 `acct02_helper` 通过 `exit_group(128)` 正常退出：

```text
[exit_current_group_and_run_next] exit_code: 128
initialize cache! /tmp/.../acct_file
```

说明记录已经写入，但 `ac_exitcode` 被写成了 0，而不是 LTP 期望的 wait status `128 << 8 = 32768`。

## 分析

内核 `wait4` 路径使用统一规则编码用户可见的 wait status：

- 普通 `exit(code)`：`code << 8`
- 信号终止：低 7 位保存信号号，core dump 时附加 `0x80`

但 `os/src/syscall/task/acct.rs` 中 accounting 记录构造单独处理了 `128..255`：

```rust
} else if exit_code >= 128 && exit_code <= 255 {
    0
} else {
    (exit_code as u32) << 8
}
```

这把合法的普通退出码 `128` 错误当成了特殊已编码状态，并写成 0。`acct02_helper` 正是正常 `exit(128)`，因此 `ac_exitcode` 校验失败。

信号终止场景不依赖这个分支；信号路径会先设置 `ProcessMeta::termination_signal`，accounting 编码会进入信号分支。

## 根因

`acct.rs` 的 `wait_status_from_exit_code()` 与 `wait4` 的状态编码规则不一致，对普通退出码 `128..255` 做了错误特殊处理。

## 修复

删除 `128..255` 特殊分支，使 accounting 普通退出状态和 `wait4` 保持一致：

```rust
fn wait_status_from_exit_code(exit_code: i32, termination_signal: Option<(usize, bool)>) -> u32 {
    if let Some((signo, dumped_core)) = termination_signal {
        signo as u32 | if dumped_core { 0x80 } else { 0 }
    } else {
        (exit_code as u32) << 8
    }
}
```

这样 `exit(128)` 写入 `32768`，信号终止仍按 `termination_signal` 写入信号状态。

## 涉及文件

| 文件 | 修改 |
|------|------|
| `os/src/syscall/task/acct.rs` | 删除 accounting exit code 对 `128..255` 的错误归零分支，普通退出统一编码为 `exit_code << 8` |

## 验证

已执行：

```text
make
timeout 120s make run > log.ans 2>&1
```

结果：

- `make` 默认 RISC-V 构建通过，仅有既有 warning。
- 当前 `initproc` 单跑 `acct02`，新的 `log.ans` 显示：

```text
acct02.c:63: TINFO: CONFIG_BSD_PROCESS_ACCT_V3=n
acct02.c:243: TINFO: Verifying using 'struct acct'
acct02.c:193: TINFO: == entry 1 ==
acct02.c:204: TINFO: Number of accounting file entries tested: 1
acct02.c:210: TPASS: acct() wrote correct file contents!
Summary:
passed   1
failed   0
broken   0
skipped  0
warnings 0
```

日志中仍有包装器行：

```text
FAIL LTP CASE acct02 : 10
```

但 LTP 本体 summary 为 `passed 1 failed 0 broken 0`，本仓库判读 LTP 结果时以 `TPASS/TFAIL/TBROK/Summary` 为准。

未执行 `TARGET_ARCH=loongarch64` 验证；本次复现和验证基于当前默认 RISC-V 配置。
