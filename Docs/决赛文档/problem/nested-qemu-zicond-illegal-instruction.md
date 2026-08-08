# 嵌套 RISC-V QEMU 的 Zicond 非法指令

## 背景

最终 RISC-V 镜像中的 `initproc` 会启动
`/opt/qemu-rv64/bin/qemu-system-riscv64`，以运行 BuildStorm 生成的
`arceos-helloworld`。该嵌套 QEMU 本身是运行在 Ya2yOS 用户态的 RISC-V ELF。

## 现象

原始 `log.ans` 中，`fstat unlink regression`、`sigaltstack regression` 与
`rseq regression` 都已通过。启动嵌套 QEMU 后，内核报告：

```text
[kernel] [hart 0] IllegalInstruction at 0x4b267e in application, kernel killed it.
```

因此 `QEMU: Terminated` 是该用户进程被终止的后果，并非根因。

## 分析

`0x4b267e` 位于嵌套 QEMU 的 `cpuinfo_init`，指令编码为 `0x0e005033`，即 Zicond
扩展的 `czero.eqz`。外层 QEMU 启动的 OpenSBI 横幅只列出 `rv64imafdch`，没有
`zicond`，执行该指令应产生 `IllegalInstruction`。

`user/src/bin/initproc.rs` 中嵌套 QEMU 的 `-cpu rv64` 参数只描述它将要模拟的下一层
guest CPU；它不影响作为 Ya2yOS 用户进程运行的嵌套 QEMU 二进制。评测机无法修改外层
QEMU 参数，因此需要由内核为这两个确定的 Zicond 指令提供兼容性模拟；不能放宽所有非法
指令的处理。

## 根因

外层 QEMU 的固定 `rv64` CPU 未启用嵌套 QEMU 二进制所需的 Zicond 扩展。

## 修复

在 RISC-V 用户态 `IllegalInstruction` 处理路径中，仅识别并模拟：

```text
czero.eqz rd, rs1, rs2
czero.nez rd, rs1, rs2
```

模拟完成后推进 `sepc` 四字节并写回 `rd`（保留 `x0` 不可写）；其他非法指令继续走原有
终止路径。外层启动参数和嵌套 QEMU 的 guest CPU 参数均不变。

嵌套 QEMU 与其 `arceos-helloworld` 产物均为 RISC-V 程序，`initproc` 仅在
`target_arch = "riscv64"` 时编译和调用该启动路径。LoongArch64 final 镜像不会尝试执行
`/opt/qemu-rv64/bin/qemu-system-riscv64`，而应继续使用其原生 CAgent/BuildStorm 流程。

## 涉及文件

- `os/src/trap/mod.rs`
- `os/src/arch/riscv64/qemu/context/trap_context.rs`
- `user/src/bin/initproc.rs`
- `Docs/决赛文档/README.md`
- `Docs/决赛文档/开发日志.md`
- `Docs/决赛文档/ai.log`
- `Docs/决赛文档/AI_INTERACTION.md`

## 验证

- `make TARGET_ARCH=riscv64`：根目录流程依次完成 RISC-V 和 LoongArch64 release 构建。
- `timeout 240s make run TARGET_ARCH=riscv64`：未添加任何 Zicond 外层 CPU 参数，三项前置
  回归均为 PASS，PID 4 不再产生 `IllegalInstruction`，并继续执行多个系统调用。
- 嵌套 QEMU 随后报告缺少
  `opensbi-riscv64-generic-fw_dynamic.bin` 并退出；这证明本次修复已越过原异常点，但
  下一层 guest 尚未启动，固件打包或 QEMU `-bios` 配置应作为独立问题处理。
