# LoongArch BuildStorm 抢占后 FCC 条件状态丢失

## 背景

LoongArch64 已启用 LSX，并在用户态陷入、定时抢占和信号返回时保存/恢复完整的
向量寄存器。BuildStorm 还会长时间并发运行 LoongArch Rust `rustc`，其浮点比较依赖
FCC 条件寄存器。

## 现象

`server.ans` 中 `sigaltstack` 和 `rseq` 回归均通过，BuildStorm 工具链与 MINIBUILD
也通过；在 `cargo build -p arceos-helloworld` 的 `core`/`compiler_builtins` 并发编译
阶段，guest 内 `rustc` 报 `interrupted by SIGSEGV`。同一时间内核报告：

```text
PagePrivilegeIllegal in application, non-canonical bad addr = 0x617461646f723e
```

该地址按小端字节解码为 `>rodata`，说明用户态控制流/内存状态已被破坏；日志中没有
内核 panic 或文件系统编译诊断可作为更早根因。

## 分析

`os/src/arch/loongarch64/qemu/asms/trap.S` 原先依次执行 `movcf2gr` 和移位，但每次
`movcf2gr` 都覆盖同一个临时寄存器，最终只把 `fcc0` 写入 TrapContext 的 FCC 字节。
返回用户态时又按 8 个 bit 解码，因此 `fcc1..fcc7` 在每次 trap/调度后被清零。

LoongArch 的 FCC 是浮点比较结果寄存器，不属于 FCSR；仅保存 LSX/FPR 和 FCSR 不能
保留这些条件状态。并发 `rustc` 的长期抢占会把丢失的比较结果暴露为错误分支，最终可能
表现为用户态非法指针和 SIGSEGV。

## 根因

FCC 保存汇编缺少逐位累积，将八个条件寄存器误写成单一 `fcc0` 状态，导致跨 trap 的
浮点条件状态污染。

## 修复

- 用 `t1` 清零累加器，将 `fcc0..fcc7` 分别移位到 bit `0..7` 后 OR 到同一字节。
- 保持现有 TrapContext 布局和返回路径不变；恢复代码继续按 bit 顺序写回八个 FCC。

## 涉及文件

- `os/src/arch/loongarch64/qemu/asms/trap.S`

## 验证

- `make build-arch TARGET_ARCH=loongarch64`：通过。
- `make build-arch TARGET_ARCH=riscv64`：通过。
- `git diff --check`：通过。

本轮未重新运行数小时的完整 LoongArch BuildStorm，因此不能仅凭构建结果宣称 `446/446`
编译已完成；应在后续运行中确认不再出现该 `PagePrivilegeIllegal`/`rustc SIGSEGV` 链路。
