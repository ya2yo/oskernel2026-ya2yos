# 嵌套 RISC-V QEMU 的 Zicond 模拟语义反转

## 背景

外层 RISC-V CPU 固定为 `rv64`，不提供 Zicond 扩展。为运行评测镜像中的
`/opt/qemu-rv64/bin/qemu-system-riscv64`，内核需要在用户态非法指令路径中兼容
`czero.eqz` 和 `czero.nez`，而不能修改 QEMU 启动参数或固定 CPU 配置。

## 现象

此前的非法指令兼容代码已经识别并跳过 Zicond 指令，因此日志中不再出现
`IllegalInstruction`，但嵌套 QEMU 仍停在启动横幅之后。调试日志显示 QEMU 的线程完成
大量 TCG 初始化和 `riscv_flush_icache` 调用，随后主线程退出而没有连续显示 guest 的
OpenSBI 和 `Hello, world!` 输出。debug 日志中的 guest UART 单字节输出会被 syscall 日志
插入，不能只用连续字符串搜索判断输出是否存在。

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

## 根因

Zicond 模拟器把“条件成立时清零”的规范语义反转成了“条件成立时选择 `rs1`”。这是一个
静默的用户态状态破坏，不会触发新的非法指令或内核 panic，因此表现为嵌套 QEMU 启动卡死。

## 修复

将选择 `rs1` 的条件改为 `(funct3 == 0x5) != (rs2 == 0)`，覆盖以下四种情况：

- `czero.eqz` 且 `rs2 == 0`：写零；
- `czero.eqz` 且 `rs2 != 0`：写 `rs1`；
- `czero.nez` 且 `rs2 != 0`：写零；
- `czero.nez` 且 `rs2 == 0`：写 `rs1`。

其他非法指令仍保持原有终止语义，未修改 QEMU 命令行、`MEMORY_SIZE`、SMP 或其他固定
配置。

## 涉及文件

- `os/src/trap/mod.rs`

## 验证

- `make log TARGET_ARCH=riscv64`：通过，debug 内核构建完成。
- `timeout 240s make run TARGET_ARCH=riscv64 > log.ans 2>&1`：通过；debug 内核运行中可见
  嵌套 guest 的 OpenSBI 与逐字符 `Hello, world!` 输出，子 QEMU 正常退出。
- `make TARGET_ARCH=riscv64`：RISC-V 和 LoongArch64 release 构建均通过。
- `timeout 240s make run TARGET_ARCH=riscv64 > log.ans 2>&1`：通过；`log.ans` 连续包含
  第二段 `OpenSBI v1.6`、`Hello, world!` 和最终 `shutdown!`，没有 `panic`、
  `IllegalInstruction` 或 `QEMU: Terminated`。
