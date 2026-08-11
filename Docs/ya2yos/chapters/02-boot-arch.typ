= 启动、架构与异常入口

== 启动序列

汇编入口位于各架构的 `arch/*/qemu/asms/entry.asm`。RISC-V 的 trampoline 完成早期地址偏移处理后进入 `rust_main(hartid)`；LoongArch 使用对应的平台入口。首个 hart 清零 BSS，初始化时钟频率，再按顺序建立核心运行环境。

#text(size: 8.5pt, fill: rgb("536471"))[_实现追溯：_ `os/src/main.rs`、`os/src/arch/*/qemu/asms/entry.asm`]

#align(center)[
  `entry.asm` → `rust_main` → `mm::init` → `logger::init` → `trap::init` → `task::init` → `fs::init` → `net::init_network` → `add_initproc` → 开启时钟中断 → `run_tasks`
]

非首 hart 等待 `INIT_FINISHED`，随后各自安装 trap 向量、激活内核页表、开启定时器并进入
调度循环。`START_HART_ID` 用于区分负责列举应用和启动初始调度的 hart；RISC-V64 与
LoongArch64 的 QEMU 配置分别声明 8 个和 12 个 hart。当前已存在远程入队、空闲 hart
通知和 IPI 路径，但调度迁移与负载均衡仍不完整。

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

== 信号返回的架构入口

信号 handler 的返回地址由 `setup_frame()` 写入 libc restorer 或架构相关的
`sigreturn_trampoline`。用户 handler 返回后，trampoline 进入 `rt_sigreturn`，由
`restore_frame()` 通过用户地址访问辅助函数读取信号帧，恢复架构 trap context、signal
mask、备用栈状态和原始返回值，再经统一的用户态返回路径继续执行。高层协议在两种
架构间共用，但 machine/trap context 布局和汇编入口分别由 `arch/riscv64/` 与
`arch/loongarch64/` 提供；因此修改 trampoline 时必须同时检查信号帧布局和 `trap.S`。

== 异常边界

异常路径对用户地址、长度和结构体执行受检访问；可修复的页故障进入懒分配、文件映射
或 COW，无法修复的访问转换为 `SIGSEGV`/`SIGBUS`。架构 IRQ 抽象仍有未完成的硬件
acknowledge/enable 路径，部分未支持 trap 和 timer condvar 分支仍可能 `panic`，不能将
统一 trap 入口表述为所有硬件异常均已实现。

RISC-V 的特权级、异常和地址转换语义以《The RISC-V Instruction Set Manual, Volume II: Privileged Architecture》为准；LoongArch 的平台差异遵循《LoongArch Architecture Reference Manual, Volume 1: Basic Architecture》。完整报告的参考资料统一列于 `main.typ` 末尾。