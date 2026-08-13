# LoongArch 用户页表内核直映别名导致 QEMU GDB 无法中断

## 背景

LoongArch64 BuildStorm 在编译 `arceos-helloworld` 时长时间停在
`Compiling arceos-helloworld v0.1.0`。现场使用 GDB 发送 Ctrl-C 和 QEMU
`Ctrl-A,X` 都没有响应，需要解释 QEMU 为什么无法进入停止边界并修复内核根因。

## 现象

采集目录 `qemu-hang-20260814-011127-pid2574` 的 host GDB 日志显示，GDB
中断请求已经被 QEMU 主线程读取，调用链为：

```text
gdb_read_byte -> vm_stop -> do_vm_stop -> pause_all_vcpus
```

主线程在 `pause_all_vcpus` 等待 CPU0 停止。CPU0 仍为
`running=1, stopped=0, stop=1, exception_index=7`，其余 11 个 vCPU 已停止，
因此主线程不能返回 GDB 事件循环，QEMU 主循环也无法处理 `Ctrl-A,X`。CPU0
快照为 `pc=CSR_TLBRERA=CSR_TLBRBADV=0xa4dd70`、
`CSR_PGDL=CSR_PGDH=0x931f3a000`、`exception_index=7`；LoongArch QEMU 中该
异常码是 `PPI`（Page Privilege error）。

## 分析

内核使用 `0x9000_0000_0000_0000` 的 DMW0 直接地址窗口，错误现场对应的高地址
`0x9000_0000_00a4dd70` 位于 `boot_stack`，而取指 PC 是低地址
`0x0000_0000_00a4dd70`。

LoongArch 当前三级页表布局下，这两个地址的 VPN 索引都为 `[0, 5, 77]`。最初
`new_from_kernel()` 复制内核根页表分支，确实会把 DMW 映射树同时暴露到用户低地址。
移除该复制后，复测仍在同一地址发生 PPI，说明还存在独立问题。

`__tlb_rfill` 无条件执行两次 `lddir` 和一次 `ldpte`。旧软件页表的未使用目录项
为零，而 LoongArch/QEMU 的 `lddir` 将目录项直接当物理地址继续读取，并不会因零
目录项停止 walk。因此空用户根表的 `root[0]` 会让 refill 继续读取物理页 0；该页中
残留的内容恰好组成指向内核 `boot_stack` 的 PLV0 PTE，于是用户态（PLV3）取指触发
PPI。`ertn` 返回原 PC 后重复该过程，vCPU 不会到达 QEMU stop boundary。

## 根因

根因有两个相互叠加的页表构造错误：

1. `new_from_kernel()` 把仅供内核 DMW0 访问的页表分支复制进每个用户地址空间。
2. 删除该复制后，未映射目录项仍为零；硬件 refill 将零当作物理页 0 的下一层表，
   而不是结束 walk。物理页 0 的残留数据可以伪造有效内核 PTE。

## 修复

1. LoongArch 用户页表不再复制内核根页表分支。内核直接地址继续由 DMW0 提供，
   不影响内核访问；`sigreturn_trampoline` 保留为显式的 `U|R|X` 用户映射。
2. 每个 LoongArch 页表建立全零叶表和指向它的空目录，根表所有未使用入口均指向
   该空目录。硬件 refill 对未映射地址最终得到无效零叶 PTE，不会访问物理页 0。
   首次映射某根/目录分支时复制私有目录或叶表，避免修改共享哨兵。
3. 不在 `trap_return()` 用 VMA-only 过滤返回 PC。该做法会误杀显式映射、但不属于
   普通 `MapArea` 的 `rt_sigreturn` trampoline `0xfffffffff0000000`；非法地址应由
   既有页故障和信号路径处理。

涉及文件：

- `os/src/arch/loongarch64/qemu/page_table.rs`

## 验证

- `make TARGET_ARCH=loongarch64`：通过，同时完成 RISC-V64 和 LoongArch64 release 构建。
- `git diff --check`：通过。
- `cargo fmt --manifest-path os/Cargo.toml -- --check`：本次文件无格式问题；检查仍报告仓库原有的 RISC-V `console.rs`、`mod.rs` 格式差异，未修改它们。
- QEMU 9.2 `target/loongarch/tcg/tlb_helper.c`：确认 `lddir` 将零目录项作为物理地址继续
  读取，零叶 PTE 对取指转换为 `PIF`；内核已将其纳入 `FetchInstructionPageFault`。
- 当前现场 QEMU PID 2574 保持 `SIGSTOP` 冻结，未恢复、终止或重启；它运行的是修复前
  的内核，不能用于验证新二进制。完整 QEMU/BuildStorm 行为回归需要使用新 `kernel-la`
  启动独立实例。
