# 嵌套 RISC-V QEMU 的 poll readiness 与 Zicond 语义修复

## 背景

外层 RISC-V CPU 固定为 `rv64`，不提供 Zicond 扩展。为运行评测镜像中的
`/opt/qemu-rv64/bin/qemu-system-riscv64`，内核需要在用户态非法指令路径中兼容
`czero.eqz` 和 `czero.nez`，而不能修改 QEMU 启动参数或固定 CPU 配置。

## 现象

此前的非法指令兼容代码已经识别并跳过 Zicond 指令，因此日志中不再出现
`IllegalInstruction`，但嵌套 QEMU 仍停在启动横幅之后。调试日志显示 QEMU 的线程完成
大量 TCG 初始化和 `riscv_flush_icache` 调用，主线程反复执行监听 stdin 和 eventfd 的
`ppoll`，但 guest 没有输出 OpenSBI 和 `Hello, world!`。

## 分析

Zicond 的两条指令都使用 R-type 编码，`funct3 == 0x5` 表示 `czero.eqz`，`funct3 == 0x7`
表示 `czero.nez`。它们的语义是：

| 指令 | 条件成立时 | 条件不成立时 |
| --- | --- | --- |
| `czero.eqz` | `rs2 == 0`，结果为 `0` | 结果为 `rs1` |
| `czero.nez` | `rs2 != 0`，结果为 `0` | 结果为 `rs1` |

旧实现用 `(funct3 == 0x5) == (rs2 == 0)` 判断写入 `rs1`，正好在上述条件成立时选择
`rs1`，在条件不成立时写零。这样虽然能让 CPU capability probe 越过非法指令，但 QEMU
后续使用 Zicond 的 TCG/设备初始化代码会得到错误的寄存器值，最终无法正常启动 guest。

同时，文件对象的 poll 实现违反了“只返回调用者请求的就绪事件”这一契约：

- `Stdin::poll(POLLIN)` 不检查 UART 是否有输入，始终返回 `POLLIN`；
- `EventFd::poll(POLLIN)` 即使调用者没有请求 `POLLOUT`，也会返回可写的 `POLLOUT`。

`ppoll` 会直接把文件对象返回的非空事件集当作 ready。嵌套 QEMU 的主事件循环只等待
stdin/eventfd 的读事件，却被上述伪就绪持续唤醒，无法按正常阻塞/唤醒节奏推进 vCPU。

## 根因

该卡死由两个必要条件共同造成：Zicond 模拟器把“条件成立时清零”的规范语义反转成
“条件成立时选择 `rs1`”；stdin/eventfd 又报告了调用者未请求或实际上不存在的就绪事件，
使 QEMU 主事件循环持续空转。两者都不会产生内核 panic，因此表面上只表现为启动横幅后
没有 guest 输出。

## 修复

将选择 `rs1` 的条件改为 `(funct3 == 0x5) != (rs2 == 0)`，覆盖以下四种情况：

- `czero.eqz` 且 `rs2 == 0`：写零；
- `czero.eqz` 且 `rs2 != 0`：写 `rs1`；
- `czero.nez` 且 `rs2 != 0`：写零；
- `czero.nez` 且 `rs2 == 0`：写 `rs1`。

其他非法指令仍保持原有终止语义，未修改 QEMU 命令行、`MEMORY_SIZE`、SMP 或其他固定
配置。

此外：

- `Stdin::poll()` 仅在实际缓存到 UART 输入时返回 `POLLIN`，poll 探测到的字节会缓存给
  后续 `read()`；同时支持 QEMU 对 stdin 设置非阻塞模式；
- `EventFd::poll()` 仅计算并返回调用者通过 `events` 请求的 `POLLIN`/`POLLOUT`。

## 涉及文件

- `os/src/trap/mod.rs`
- `os/src/fs/files/stdio.rs`
- `os/src/fs/files/events.rs`

## 验证

- `make log TARGET_ARCH=riscv64`：通过，debug 内核构建完成。
- `timeout 240s make run TARGET_ARCH=riscv64 > log.ans 2>&1`：通过；debug 内核运行中可见
  嵌套 guest 的 OpenSBI 与逐字符 `Hello, world!` 输出，子 QEMU 正常退出。
- `make build-arch TARGET_ARCH=riscv64`：每轮减法对照均明确显示 `Compiling os` 并重新
  生成 `kernel-rv`。
- `timeout 240s make run TARGET_ARCH=riscv64 > log.ans 2>&1`：仅保留 Zicond 时卡死；加入
  进程级 icache 仍卡死；加入 stdin/eventfd poll 修复后通过；再次移除进程级 icache 仍
  通过，证明 icache、madvise 和 MM 尝试不是本问题的必要修改。
- 最终 `log.ans` 连续包含
  第二段 `OpenSBI v1.6`、`Hello, world!` 和最终 `shutdown!`，没有 `panic`、
  `IllegalInstruction` 或 `QEMU: Terminated`。
