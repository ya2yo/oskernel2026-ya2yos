= 启动、架构与异常入口

== 启动序列

汇编入口位于各架构的 `arch/*/qemu/asms/entry.asm`。RISC-V 的 trampoline 完成早期地址偏移处理后进入 `rust_main(hartid)`；LoongArch 使用对应的平台入口。首个 hart 清零 BSS，初始化时钟频率，再按顺序建立核心运行环境。

#text(size: 8.5pt, fill: rgb("536471"))[_实现追溯：_ `os/src/main.rs`、`os/src/arch/*/qemu/asms/entry.asm`]

#align(center)[
  `entry.asm` → `rust_main` → `mm::init` → `logger::init` → `trap::init` → `task::init` → `fs::init` → `net::init_network` → `add_initproc` → 开启时钟中断 → `run_tasks`
]

非首 hart 等待 `INIT_FINISHED`，随后各自安装 trap 向量、激活内核页表、开启定时器并进入调度循环。`START_HART_ID` 用于区分负责列举应用和启动初始调度的 hart。当前默认配置中的 `HART_NUM` 为 1，但启动屏障保留了多 hart 的初始化骨架。

== 架构抽象

`os/src/arch/mod.rs` 是与上层交互的汇聚点。两个架构分别提供：

- 任务和 trap 寄存器上下文，以及 `switch.S` 完成的上下文切换；
- 页表建立、地址翻译和 TLB 刷新；
- 时钟频率初始化、下一次时钟触发和中断开关；
- UART 控制台、链接脚本、物理内存布局和启动汇编；
- RISC-V 的 SBI 路径，以及 LoongArch 的 PCI/VirtIO 平台接入。

架构相关代码应保持在 `arch/` 或驱动的架构子目录中。上层不直接写 CSR 或平台寄存器，而经 `trap_interface`、`time`、`cpu` 和页表接口调用。

== Trap 到用户态返回

`trap::trap_handler()` 是用户态进入内核的统一门。它从当前线程取得 trap context，根据异常原因选择系统调用、页故障、定时器中断或其他异常处理，并在准备恢复用户寄存器前检查待处理信号。

#table(
  columns: (1.1fr, 2fr, 1.45fr),
  table.header([*事件*], [*主要处理*], [*可见结果*]),
  [用户 `ecall`], [递增返回地址，读取 syscall 号和 6 个参数，调用 `syscall()`], [返回值写回用户寄存器],
  [load/store/instruction page fault], [查询 `MemorySet` 的 VMA，执行懒分配、COW 或文件页处理], [成功后重试；非法访问转换为异常信号],
  [timer interrupt], [更新时间、设置下一次触发，驱动抢占/唤醒相关路径], [可切换到其他 ready 任务],
  [外部中断], [经架构中断接口和设备驱动分发], [设备状态推进或唤醒等待者],
)

异常路径不能信任用户提供的地址、长度或结构体。所有跨地址空间读写使用内存模块的受检辅助函数，错误以 Linux errno 或适当信号反馈给用户程序。

RISC-V 的特权级、异常和地址转换语义以 RISC-V 特权架构规范为准 #cite(<riscv-privileged>)；LoongArch 的平台差异遵循其基础架构手册 #cite(<loongarch-volume1>)。
