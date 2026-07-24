# LTP mmap18 MAP_GROWSDOWN 与 SIGSEGV 线程组退出修复

## 背景

LTP `mmap18` 覆盖匿名 `MAP_GROWSDOWN` 映射的两组语义。它先预留
`256 * page_size + stack_size` 的地址空间，解除映射后在最高端以
`MAP_FIXED | MAP_PRIVATE | MAP_ANONYMOUS | MAP_GROWSDOWN` 映射少量页，再将这段区域作为
pthread 栈递归使用。前两轮要求栈在空洞中向低地址扩展；后两轮在预期扩展区域先固定映射一页，
要求子进程因 `SIGSEGV` 终止。

测试源码位于只读测试集
`testsuits-for-oskernel/ltp-full-20240524/testcases/kernel/syscalls/mmap/mmap18.c`。

## 现象

早期 musl trace 先通过两项正例，随后在负例卡到 LTP 30 秒 watchdog：

```text
mmap18.c:165: TPASS: Stack grows in unmapped region
mmap18.c:165: TPASS: Stack grows in unmapped region
Test timeouted, sending SIGKILL!
TBROK: Test killed! (timeout?)
passed   2
broken   1
```

最终 RISC-V `log.ans` 中，musl 与 glibc 都有两项栈扩展和两项预期 `SIGSEGV`：

```text
mmap18.c:165: TPASS: Stack grows in unmapped region
mmap18.c:196: TPASS: Child killed by SIGSEGV as expected
```

两轮摘要均为 `passed 4 failed 0 broken 0 warnings 0`，并正常输出 `shutdown!`。
musl 包装器中的 `FAIL LTP CASE mmap18 : 0` 是无条件打印的历史标签，末尾状态为 0，
不是失败；`scripts/extract_failed_tests.py log.ans` 也报告未发现失败测试。

## 分析

`MAP_GROWSDOWN` 标志已能被 syscall 参数解析，但原实现没有在缺页地址低于 VMA 起始页时扩展
VMA。缺页处理只会命中包含 fault VPN 的 mmap、brk 或固定 stack 区域，因此 guard page 访问会
直接返回失败并由 trap 层送出 `SIGSEGV`。

负例还暴露了独立的信号语义缺口。默认 `Terminate/CoreDump` 动作此前只调用
`exit_current_and_run_next()`，只结束触发 `SIGSEGV` 的 pthread。Linux 的默认致命信号应终止
整个线程组；否则测试子进程不会以 `WIFSIGNALED(SIGSEGV)` 的形式被父进程观察到，musl 在
`pthread_join()` 路径中最终被 watchdog 杀死。

将所有默认致命信号改为线程组退出后，`execve` 的去线程化路径需要特殊处理：
`kill_other_threads_before_exec()` 也用内部 `SIGKILL` 清理 sibling，但它不能终止即将替换映像的
exec 调用线程。仅靠“没有 `SigInfo`”无法区分该内部信号和 strict seccomp 的内核 `SIGKILL`，
因此需要显式来源标记。

## 根因

1. 匿名私有 `MAP_GROWSDOWN` VMA 的 guard page 缺页未进入 VMA 向下扩展路径。
2. 默认 `SIGSEGV` 只结束当前线程而不是线程组，且没有区分 execve sibling 清理使用的内部
   `SIGKILL`。

## 修复

- `os/src/mm/memory_set/mmap_ops.rs` 在普通 VMA、brk 和 stack 缺页查找均未命中后，查找最近的
  匿名私有 `MAP_GROWSDOWN` VMA。扩展前拒绝已映射地址、穿过其他 VMA 的增长，以及距下方 VMA
  少于 256 页的增长；成功使用原 mmap 缺页权限/EOF 处理分配页面后，再把 VMA 起始 VPN 下移。
- `os/src/signal/pending.rs` 将默认 `Terminate/CoreDump` 动作改为
  `exit_current_group_and_run_next()`，保留首个致命信号供 `waitpid()`/`waitid()` 编码。
- `os/src/signal/delivery.rs`、`os/src/task/task/task.rs` 和 `os/src/task/mod.rs` 增加每任务
  `exec_teardown_kill` 与专用投递来源。普通或用户态 `SIGKILL` 覆盖该标记并走线程组终止；仅
  execve 标记的 sibling 清理保持 `exit_current_and_run_next(137)`。
- `os/src/syscall/mod.rs` 为 strict seccomp 的内核 `SIGKILL` 预先保存信号终止原因，避免调度快速
  路径把它错误编码为普通 `exit(137)`。
- `os/src/syscall/options.rs` 删除 `MAP_GROWSDOWN` 为 no-op 的过时注释。

## 涉及文件

| 文件 | 修改 |
|---|---|
| `os/src/mm/memory_set/mmap_ops.rs` | `MAP_GROWSDOWN` guard page 的受限 VMA 扩展 |
| `os/src/signal/pending.rs` | 默认致命信号改为线程组退出 |
| `os/src/signal/delivery.rs` | execve 内部 SIGKILL 的来源标记 |
| `os/src/task/mod.rs` | execve sibling 清理与 SIGKILL 快速退出分流 |
| `os/src/task/task/task.rs` | 保存每任务内部清理标记 |
| `os/src/syscall/mod.rs` | strict seccomp SIGKILL 的 wait status 保留 |
| `os/src/syscall/options.rs` | 更新 flag 语义注释 |

## 验证

已执行：

```text
make log TARGET_ARCH=riscv64
timeout 90s make run TARGET_ARCH=riscv64 > log.ans 2>&1
rg -a -n "TPASS|TFAIL|TBROK|panic|ERROR|WARN|Summary" log.ans
python3 scripts/extract_failed_tests.py log.ans
git diff --check
```

RISC-V debug 构建成功。最终根目录 `log.ans` 中 musl/glibc 各有 4 个 `TPASS`，无
`TFAIL`、`TBROK`、panic、ERROR 或 WARN，QEMU 正常 `shutdown!`；失败提取脚本输出
“未发现失败测试”，`git diff --check` 通过。

未运行 LoongArch64 运行时回归、完整 LTP 批量套件、独立的多线程 execve 或 strict seccomp
回归；本次运行聚焦 `mmap18` 触发路径。
