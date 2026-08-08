# riscv_flush_icache(259) 系统调用

## 背景

RISC-V Linux ABI 将 259 号系统调用定义为 `riscv_flush_icache(2)`，用于在生成或修改可执行代码后使 instruction cache 观察到最新内容。

## 现象

项目原有 RISC-V syscall 枚举和分发缺少 259 号入口，用户态调用会落入未实现系统调用分支并返回 `ENOSYS`。

## 分析

RISC-V 架构代码已经提供 `instruction_fence()`，在当前 hart 执行 `fence.i`。LoongArch64 也提供同名架构接口，但 259 仅属于 RISC-V syscall ABI，因此 syscall 枚举、分发和用户态封装需要使用 RISC-V 条件编译。

## 根因

缺少 `RiscvFlushIcache = 259` 枚举项及其 syscall handler 接入。

## 修复

- 增加 RISC-V 专用 `RiscvFlushIcache = 259`。
- 新增 `sys_riscv_flush_icache()`，校验 flags，仅接受普通模式和 `SYS_RISCV_FLUSH_ICACHE_LOCAL`。
- 复用架构层 `instruction_fence()` 执行当前 hart 的 instruction fence。
- 地址范围参数按 ABI 接收；当前架构接口只提供整 hart fence，因此不进一步细分范围。
- 增加 RISC-V 条件编译的用户态封装。

## 涉及文件

- `os/src/syscall/mod.rs`
- `os/src/syscall/sys/riscv.rs`
- `user/src/syscall/mod.rs`

## 验证

执行 `make TARGET_ARCH=riscv64` 和 `make TARGET_ARCH=loongarch64`，双架构 release 构建通过。未单独运行 QEMU/LTP 的 259 号直接调用测试。
