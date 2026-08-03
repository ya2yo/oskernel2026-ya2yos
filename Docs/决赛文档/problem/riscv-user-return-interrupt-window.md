# RISC-V 返回用户态的 S-mode 中断窗口

## 背景

RISC-V `trap_return()` 在准备用户 `TrapContext` 后，通过 `set_user_trap_entry()` 将 `stvec`
切换到 `__trap_from_user`，再由 `__return_to_user` 恢复寄存器并执行 `sret`。`__trap_from_user`
假定陷入来自 U-mode，且 `sscratch` 指向用户 `TrapContext`。

## 现象

旧顺序在仍处于 S-mode 时较早设置 user trap entry，之后汇编又恢复保存的 `sstatus`。若新建、旧存或其他
异常 `TrapContext` 带有 `SIE=1`，定时器等中断可以发生在 `stvec` 已经指向 user handler、但尚未 `sret`
的短窗口内。此时会按 U-mode 的 stack/`sscratch` 约定处理一个实际 S-mode 中断，存在覆盖 TrapContext、错误
hart id 或破坏返回现场的风险。

## 根因

返回路径没有把“设定 user trap vector”“恢复可开中断的 `sstatus`”和“执行 `sret`”作为一个不可中断的
交接序列。特别是 `sstatus` 中 `SIE` 在 `sret` 前对当前 S-mode 就生效，而 `SPIE` 才是 `sret` 后用户态的
中断使能来源。

## 修复

- Rust `trap_return()` 在准备 `TrapContext` 时显式 `clear_sie()`。
- `set_user_trap_entry()` 移到紧邻非返回 `__return_to_user()` 调用的位置，缩短 vector 已切换的窗口。
- `__return_to_user` 在写回保存的 `sstatus` 后执行 `csrci sstatus, 2`，强制清除当前 S-mode 的 `SIE`；
  不修改 `SPIE`，因此 `sret` 后仍按 TrapContext 中预期的用户中断状态恢复。

该修改只覆盖 RISC-V return-to-user 汇编路径，不改变用户态中断是否启用的既有语义。

## 涉及文件

- `os/src/trap/mod.rs`
- `os/src/arch/riscv64/qemu/asms/trap.S`

## 验证

已完成 Rust 格式化与 `git diff --check`。`make TARGET_ARCH=riscv64` 在 lwext4 CMake 阶段被宿主缺少
`riscv64-linux-musl-cc` 阻断，未完成链接和 QEMU 启动。

尚未在新内核上完成用户态高频中断/信号压力、BuildStorm 或 LTP 运行验证；LoongArch64 不使用此汇编路径，
不应据此推导跨架构运行结果。
