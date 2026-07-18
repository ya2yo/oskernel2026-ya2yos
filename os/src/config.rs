// CPU数量。
//
// RISC-V QEMU 使用 SBI HSM 启动两个 hart。调度器按进程固定亲和性分配任务，
// 因此在远程 TLB shootdown 完成前，同一地址空间不会同时出现在两个 hart 上。
pub const HART_NUM: usize = 2;
// 线程最大数量
pub const THREAD_MAX_NUM: usize = 3000;
