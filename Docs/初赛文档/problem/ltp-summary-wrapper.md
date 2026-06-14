# LTP 包装层 Summary 汇总

## 背景

最终跑分入口会调用 `test_musl_ltp()` / `test_glibc_ltp()`，这些函数通过 `run_ltp_tests_*()` 批量 fork/exec LTP 测例。部分旧式 LTP 测例只输出 `TPASS/TFAIL/TBROK/TCONF` 行，未必在当前运行方式下输出统一 `Summary:`。

`munmap01` 是典型例子：测例会 `mmap` 一段文件映射，`munmap` 后故意写已解除映射地址，依赖 SIGSEGV handler 记录 `TPASS`。

## 现象

日志中 `munmap01` 已打印：

```text
munmap01    1  TPASS  :  Functionality of munmap() successful
```

随后完成 cleanup 并以退出码 0 结束，但外层没有 Summary。单独只修 `test_musl_single()` 没有意义，因为最终跑分使用批量入口。

## 分析

从 `log.ans` 看，`munmap` 返回 0 后的 StorePageFault 是测例预期行为；信号 handler 正常执行，测例继续清理临时目录并 `exit_group(0)`。因此根因不在内核 `munmap` 语义或信号恢复路径。

LTP 旧接口会用退出码 bit 表达类型，常见类型包括：

- `TFAIL = 0x01`
- `TBROK = 0x02`
- `TWARN = 0x04`
- `TCONF = 0x20`

Ya2yOS `waitpid` 对普通退出返回 Linux 风格 `exit_code << 8`，外层包装需要先还原退出码，再按类型累计 Summary。

## 根因

`run_ltp_tests_musl*()` / `run_ltp_tests_glibc()` 只打印每个子进程 wait status，没有按 LTP 退出类型累计 `passed/failed/broken/skipped`，导致旧式测例在批量跑分入口下缺少统一 Summary。

第一版补充 Summary 时只根据 wait status 和 LTP exit code 统计，`exit_code == 0` 只会把整个测例计为 1 个 `passed`。这与 LTP 自身输出不一致：一个测例内可能打印多条 `TPASS`，也可能同时打印 `TPASS` 和 `TFAIL`，最终汇总应反映输出中的实际断言数量。

## 修复

在 `user/src/bin/ltp/mod.rs` 中新增 `LtpSummary`：

- 退出码 0 计 `passed`
- `TFAIL` 计 `failed`
- `TBROK` 计 `broken`
- `TCONF` 和黑名单跳过计 `skipped`
- `TWARN` 计 `warnings`
- 非标准非零退出计 `broken`

`run_ltp_tests_musl()`、`run_ltp_tests_musl_separately()`、`run_ltp_tests_glibc()` 在每个 group 结束时输出 Summary；`test_musl_single()` / `test_glibc_single()` 复用同一套统计逻辑。同时打印测试名时使用 `trim_trailing_nul()`，避免日志中出现 NUL。

后续将 LTP 运行改为 `fork_run_ltp_and_collect()`：

- 子进程 stdout/stderr 通过 pipe 返回父进程
- 父进程边读 pipe 边原样写回 stdout，保持日志可见
- 流式扫描输出中的 `TPASS/TFAIL/TBROK/TCONF/TWARN`
- `LtpSummary` 优先累计输出 token 数量，只有未识别到 LTP token 时才退回旧的 wait status 统计

## 涉及文件

- `user/src/bin/ltp/mod.rs`

## 验证

已执行：

```text
make
timeout 90s make run
```

结果：

```text
munmap01    1  TPASS  :  Functionality of munmap() successful
RESULT MUSL LTP SINGLE CASE munmap01 : 0
Summary:
passed   1
failed   0
broken   0
skipped  0
warnings 0
```

`make run` 正常 shutdown，命令退出码为 0。

2026-06-14 继续验证按输出 token 统计：

```text
abort01.c:62: TPASS: abort() dumped core
abort01.c:65: TPASS: abort() raised SIGIOT

Summary:
passed   2
```

批量入口下，后续 `accept01` 输出 5 条 `TPASS` 后累计 `passed` 从 2 增加到 7；`access04` 输出 9 条 `TPASS`、3 条 `TFAIL`、1 条 `TWARN` 后，累计 Summary 对应增加。`timeout 90s make run` 因全量 LTP 未跑完被外部 timeout 结束，但已覆盖多种 `TPASS/TFAIL/TBROK/TCONF/TWARN` 统计路径。
