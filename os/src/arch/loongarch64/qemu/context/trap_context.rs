use super::regs::*;
use crate::{
    arch::__PAD_SIZE,
    signal::{SigSet, SignalStack},
};
use core::fmt::Debug;
use loongArch64::register::{prmd, CpuMode};

const PRMD_PPLV_MASK: usize = 0b11;
const PRMD_PIE: usize = 1 << 2;

fn user_return_prmd(bits: usize) -> prmd::Prmd {
    let bits = (bits & !PRMD_PPLV_MASK) | CpuMode::Ring3 as usize | PRMD_PIE;
    bits.into()
}
#[repr(C)]
#[derive(Default, Debug, Clone, Copy)]
pub struct MachineContext {
    gp: GeneralRegs,
    fp: FloatRegs,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct UserContext {
    pub flags: usize,
    pub link: usize,
    pub stack: SignalStack,
    pub sigmask: SigSet,
    pub __pad: [u8; __PAD_SIZE],
    pub mcontext: MachineContext,
}

#[repr(C)]
#[derive(Clone, Copy)]
/// The trap cotext containing the user context and the supervisor level
pub struct TrapContext {
    /// The registers to be preserved.
    gp: GeneralRegs,
    fp: FloatRegs,
    /// A copy of register a0, useful when we need to restart syscall
    pub origin_a0: usize,
    /// Privilege level of the trap context
    sstatus: prmd::Prmd,
    /// The current sp to be recovered on next entry into kernel space.
    pub kernel_stack: usize,
}

// `trap.S` saves the user context with fixed byte offsets. Keep the Rust
// layout checked at compile time so a future field change cannot silently
// corrupt LSX state or the return frame.
const _: () = {
    assert!(core::mem::size_of::<usize>() == 8);
    assert!(core::mem::size_of::<GeneralRegs>() == 32 * 8);
    assert!(core::mem::align_of::<FloatRegs>() == 16);
    assert!(core::mem::size_of::<FloatRegs>() == 66 * 8);
    assert!(core::mem::offset_of!(TrapContext, fp) == 32 * 8);
    assert!(core::mem::offset_of!(TrapContext, origin_a0) == 98 * 8);
    assert!(core::mem::offset_of!(TrapContext, sstatus) == 99 * 8);
    assert!(core::mem::offset_of!(TrapContext, kernel_stack) == 100 * 8);
};

impl TrapContext {
    pub fn app_init_context(entry: usize, sp: usize, kernel_sp: usize) -> Self {
        let mut cx = Self {
            gp: GeneralRegs::default(),
            fp: FloatRegs::default(),
            origin_a0: 0,
            sstatus: user_return_prmd(0),
            kernel_stack: kernel_sp,
        };
        cx.gp.pc = entry;
        cx.set_sp(sp);
        cx
    }

    /// Force the architectural state required by every return to userspace.
    pub fn prepare_user_return(&mut self) {
        self.sstatus = user_return_prmd(self.sstatus.raw());
    }

    pub fn as_mctx(&self) -> MachineContext {
        MachineContext {
            gp: self.gp,
            fp: self.fp,
        }
    }
    pub fn copy_from_mctx(&mut self, mctx: MachineContext) {
        self.gp = mctx.gp;
        self.fp = mctx.fp;
    }
    pub fn get_syscall_id(&self) -> usize {
        self.gp.a7
    }

    pub fn get_syscall_args(&self) -> [usize; 6] {
        [
            self.gp.a0, self.gp.a1, self.gp.a2, self.gp.a3, self.gp.a4, self.gp.a5,
        ]
    }
    pub fn get_a0(&self) -> usize {
        self.gp.a0
    }

    pub fn set_a0(&mut self, val: usize) {
        self.gp.a0 = val;
    }

    pub fn get_a1(&self) -> usize {
        self.gp.a1
    }

    pub fn set_a1(&mut self, val: usize) {
        self.gp.a1 = val;
    }

    pub fn get_a2(&self) -> usize {
        self.gp.a2
    }

    pub fn set_a2(&mut self, val: usize) {
        self.gp.a2 = val;
    }

    pub fn get_sepc(&self) -> usize {
        self.gp.pc
    }

    /// Return the raw saved status register value used when returning to user mode.
    pub fn get_status_bits(&self) -> usize {
        self.sstatus.raw()
    }

    pub fn set_sepc(&mut self, val: usize) {
        self.gp.pc = val
    }

    pub fn sepc_step(&mut self, val: isize) {
        self.gp.pc += val as usize
    }

    pub fn get_sp(&self) -> usize {
        self.gp.sp
    }

    pub fn set_sp(&mut self, sp: usize) {
        self.gp.sp = sp;
    }

    pub fn get_tp(&self) -> usize {
        self.gp.tp
    }

    pub fn set_tp(&mut self, tp: usize) {
        self.gp.tp = tp;
    }

    pub fn get_ra(&self) -> usize {
        self.gp.ra
    }

    pub fn get_t0(&self) -> usize {
        self.gp.t0
    }

    pub fn set_ra(&mut self, ra: usize) {
        self.gp.ra = ra;
    }

    /// 用户态 frame pointer（r22），用于临时诊断时手动回溯用户栈。
    pub fn get_fp(&self) -> usize {
        self.gp.fp
    }
}

impl Debug for TrapContext {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("TrapContext")
            .field("gp", &self.gp)
            .field("fp", &self.fp)
            .field("origin_a0", &self.origin_a0)
            // .field("sstatus", &self.sstatus)
            .field("kernel_sp", &format_args!("{:#x}", self.kernel_stack))
            .finish()
    }
}
