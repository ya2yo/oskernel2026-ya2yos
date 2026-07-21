# LoongArch 八核 basic clone 的 COW 源帧并发释放

## 背景

LoongArch QEMU 扩展到 `8G / 8 CPU` 后，独立进程按
`(pid - 1) % HART_NUM` 固定到不同 home hart。线程仍不跨 hart 迁移，但 fork
父子拥有不同 `MemorySet`，可以在不同 hart 同时处理同一共享物理页的 COW 写故障。

维护者发现相同内核和测试入口每次运行结果不同，并提供根目录 `log.ans` 要求分析和修复。
当前入口依次执行 pre_tests 镜像的 `basic-musl` 与 `basic-glibc`。

## 现象

原始日志中 `basic-musl` 完整结束；`basic-glibc` 运行到 `test_clone` 时出现：

```text
[WARN] [HART4] [PID 53] [TID 53] [kernel] [hart 4] IllegalInstruction at 0x25d0 in application, kernel killed it.
clone process successfully.
pid:53
========== END test_clone ==========
Testing close :
Segmentation fault
```

这里父进程打印 `clone process successfully` 只表示 `wait()` 返回了目标 PID，并没有验证子进程
退出状态。下一个 `test_close` 甚至没有输出自己的 `START`，说明后续段错误发生在复用同一 shell
的 fork/exec 边界，而不是 `close(2)` 的固定断言。

日志没有内核 panic、`TFAIL`、`TBROK`、陈旧调度项或重复入队告警。PID 53 按新的 home-hart
公式满足 `(53 - 1) % 8 = 4`，与故障日志的 HART4 精确一致。

## 分析

从只读 pre_tests ext4 镜像检查 `/glibc/basic/clone` 后确认，故障 PC `0x25d0` 是合法的
LoongArch 基础指令，紧接 `exit(93)` syscall：

```asm
25d0: 0381740b  ori     $a7, $zero, 93
25d4: 002b0000  syscall 0
```

musl 与 glibc 目录中的 clone 二进制 SHA-256 相同，且同一个程序已在前一组正常运行，因而这不是
镜像差异或 QEMU 缺少指令扩展。

该测试 ELF 只有一个覆盖 `[0, 0x48d0)` 的 RWE `LOAD` 段。故障指令 `0x25d0`、全局变量
`child_pid = 0x2788` 以及从 `0x2790` 开始的 clone 子栈都位于 VPN `0x2`。clone 返回后，
父进程写 `child_pid`，子进程在自己的栈上写入；两者会在不同 hart 同时拆分这个 COW 页，而该页
还承载正在执行的代码。

LoongArch 原 `PageTable::handle_write_protect_page_fault()` 的共享分支按以下顺序工作：

```text
读取 Arc::strong_count
从 PTE 取得裸源页 slice
unmap_one() 删除当前 VMA 的 Arc<FrameTracker>
分配并清零目标帧
从此前取得的裸源页复制
```

单个地址空间的写故障受自己的 `MemorySet` 写锁保护，但父子进程有两把不同的锁。两个 hart
可同时取得裸源地址并各自删除 VMA 引用；最后一个 `FrameTracker` 析构后，frame allocator 会
回收该物理页。后续分配可能立即复用并清零它，另一 hart 却仍从已经失去所有权的裸地址复制，
于是得到随机损坏的代码、栈或数据页。

典型交错如下：

```text
hart A: 取得共享页的裸 src
hart B: 取得共享页的裸 src
hart A: unmap_one()，删除自己的 FrameTracker 引用
hart B: unmap_one()，删除最后一个 VMA 引用并回收源页
hart B: 分配器复用并清零该页
hart A: 从已复用的 src 复制，安装损坏的新页
```

RISC-V SMP 曾出现同型问题，且其页表实现已经采用源帧 pin 和“先复制、后换 PTE”的顺序；
LoongArch 在启用多 hart 前仍保留旧实现，因此单核时代没有暴露这个窗口。

## 根因

根因是 LoongArch COW 复制只保留了源物理页号和裸 slice，没有让
`Arc<FrameTracker>` 的生命周期覆盖复制过程。不同地址空间的 COW handler 可以并发删除最后
两个 VMA 引用，使源帧在仍被读取时释放、清零并复用。这是物理页所有权竞态，不是固定非法指令、
普通 TLB 未刷新或 clone ABI 错误。

## 修复

`os/src/arch/loongarch64/qemu/page_table.rs` 现在采用以下顺序：

1. 在临时 pin 前读取原始 `Arc::strong_count`，保留独占页快速路径的判断语义；
2. 克隆 `source_frame`，并校验其 PPN 与当前 PTE 一致；
3. 保持旧 PTE 和旧 VMA 引用有效，先分配新帧并复制完整源页；
4. 用新 PPN 和去除 COW、加入 writable/dirty 的 flags 一次替换 PTE；
5. 刷新本 hart TLB 后，用新帧替换 `data_frames` 所有权，最后释放旧 VMA 引用和临时 pin。

这样父子即使同时拆分同一页，各自的局部 `source_frame` 也会阻止源物理页在复制完成前被回收。
同时，目标帧分配失败时旧 PTE 和 VMA 保持完整，不再留下已经解除映射却无法恢复的地址空间。

## 涉及文件

- `os/src/arch/loongarch64/qemu/page_table.rs`
- `Docs/决赛文档/problem/loongarch-cow-source-frame-race.md`
- `Docs/决赛文档/problem/README.md`
- `Docs/决赛文档/开发日志.md`
- `Docs/决赛文档/ai.log`
- `Docs/决赛文档/AI_INTERACTION.md`

维护者已有的 `Makefile`、`make_scripts/loongarch64.mk` 和 `user/src/bin/initproc.rs` 修改只用于
定义当前 8 核复现环境，本次没有回退或重写这些改动。

## 验证

执行：

```bash
make TARGET_ARCH=loongarch64
timeout 90s make run TARGET_ARCH=loongarch64
```

LoongArch64 user/kernel release 构建通过，只有 allocator 与 vendored `smoltcp` 的既有 warning。
在 `8G / 8 CPU`、pre_tests 镜像、`basic-musl + basic-glibc` 入口下连续执行五轮 QEMU：

- 五轮均启动全部 8 个 hart；
- 每轮两组各 32 个 basic 测试均完成；
- 每轮两个 `test_clone` 都输出 `Child says successfully!`；
- 每轮均输出两个 `GROUP END` 和 `shutdown!`；
- 五轮均未出现 `IllegalInstruction`、`Segmentation fault`、panic、`TFAIL`、`TBROK` 或 `ERROR`。

前三个保存样本中的每个日志均为 474 行；其中 `START test_` 共 64 个，普通 `END test_`
共 62 个，另两个 `execve` 按测例自身格式输出 `END main`。这不是测试缺失。

`cargo fmt --manifest-path os/Cargo.toml -- --check` 仍报告既有
`os/src/arch/loongarch64/qemu/mod.rs` 模块声明顺序差异；本次页表文件通过 `git diff --check`，
没有为消除无关格式差异而修改该文件。

本次只改 LoongArch 架构页表实现，未运行 RISC-V QEMU 行为回归。

## 剩余边界

`data_frames` 中没有 tracker 的历史 Brk COW PTE 仍无法取得真正的源帧 pin；当前沿用既有逻辑，
强制进入复制分支以保留数据。长期应保证每个受管 present PTE 都能查到稳定的 frame owner，避免
任何 COW 分支依赖裸 PPN 的生命周期假设。

当前正确性仍依赖同一地址空间固定在单一 home hart。本修复不实现任务迁移、远程 TLB shootdown
或通用 reschedule IPI；未来放开同一地址空间跨 hart 运行前，必须先补齐远程 TLB 一致性。
