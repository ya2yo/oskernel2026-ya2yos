//! 定义提供给外界的tarp类型

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trap {
    Exception(Exception),
    Interrupt(Interrupt),
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exception {
    /// This exception is triggered when the virtual address of a LOAD(i.e. `ld.{b,h,w,d}`) operation finds a match in the TLB with `V=0`.
    LoadPageFault,
    /// This exception is triggered when the virtual address of a STORE(i.e. `st.{b,h,w,d}`) operation finds a match in the TLB with `V=0`
    StorePageFault,
    /// This exception is triggered when the virtual address of an instruction fetching  operation finds a match in the TLB with `V=0`.
    /// InstructionPageFault/FetchPageFault
    FetchInstructionPageFault,
    /// 龙芯特有的PageModifyFault，发生时需要内核将这一页的dirty置为1
    PageModifyFault,
    /// Page privilege level illegal (LoongArch: page present but privilege check fails).
    /// Triggered when the virtual address matches a TLB entry with V=1 but the
    /// privilege level is insufficient for the attempted access.
    PagePrivilegeIllegal,
    /// Illegal or unsupported instruction from user space.
    IllegalInstruction,
    /// system call （在riscv64版本中等价于UserEnvCall）
    Syscall,
}

/// The interrupt type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(usize)]
pub enum Interrupt {
    ///Timer Interrupt 在riscv64版本中等价于SupervisorTimer
    Timer,
    /// Inter-processor interrupt used for scheduler wakeups and remote
    /// translation-cache shootdown.
    Ipi,
}
