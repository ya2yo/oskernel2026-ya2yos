use super::regs::*;
use crate::trap::trap_return;
use riscv::register::sstatus::{self, Sstatus, SPP};
use crate::arch::__PAD_SIZE;
#[repr(C)]
#[derive(Debug, Clone, Copy)]
///trap context structure containing sstatus, sepc and registers
pub struct TrapContext {
    gp: GeneralRegs,      // 0-31
    pub sstatus: Sstatus, // 32
    sepc: usize,          // 33
    /// 内核栈的最高地址处，而不是内核的“当前栈顶”的位置
    pub kernel_stack: usize, // 34
    /// 等于mhartid
    pub kernel_hartid: usize, // 35
    /// A copy of register a0, useful when we need to restart syscall
    pub origin_a0: usize, // 35
    pub fp: FloatRegs,    // 37-70(含fcsr)
}

// Keep the Rust ABI contract explicit: trap.S accesses these offsets before
// Rust code can validate the saved context.  A field reordering or alignment
// change must therefore fail the build rather than silently corrupt tp/sp.
const _: () = {
    assert!(core::mem::offset_of!(TrapContext, gp) == 0);
    assert!(core::mem::offset_of!(TrapContext, sstatus) == 32 * 8);
    assert!(core::mem::offset_of!(TrapContext, sepc) == 33 * 8);
    assert!(core::mem::offset_of!(TrapContext, kernel_stack) == 34 * 8);
    assert!(core::mem::offset_of!(TrapContext, kernel_hartid) == 35 * 8);
    assert!(core::mem::offset_of!(TrapContext, origin_a0) == 36 * 8);
    assert!(core::mem::offset_of!(TrapContext, fp) == 37 * 8);
};

use super::regs::*;
use crate::signal::{SigSet, SignalStack};
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

impl TrapContext {
    ///init app context
    pub fn app_init_context(entry: usize, sp: usize, kernel_sp: usize) -> Self {
        let mut sstatus = sstatus::read();
        sstatus.set_spp(SPP::User);
        let mut cx = Self {
            gp: GeneralRegs { x: [0; 32] },
            sstatus,
            sepc: entry,
            kernel_stack: kernel_sp,
            kernel_hartid: 0, // TODO: 这对吗？
            origin_a0: 0,
            fp: FloatRegs {
                f: [0; 32],
                fcsr: 0,
            },
        };
        cx.set_sp(sp);
        cx
    }
    pub fn as_mctx(&self) -> MachineContext {
        let mut x = [0; 32];
        x.copy_from_slice(&self.gp.x);
        let mut f = [0; 32];
        f.copy_from_slice(&self.fp.f);
        let fcsr = self.fp.fcsr;
        x[0] = self.sepc; // x0 寄存器永远为0,暂时借用一下,用于保存sepc
        MachineContext {
            gp: GeneralRegs { x },
            fp: FloatRegs { f, fcsr },
        }
    }
    pub fn copy_from_mctx(&mut self, mctx: MachineContext) {
        self.gp = mctx.gp;
        self.fp = mctx.fp;
        self.sepc = self.gp.x[0];
        self.gp.x[0] = 0;
    }

    pub fn get_syscall_id(&self) -> usize {
        self.gp.x[17]
    }

    pub fn get_syscall_args(&self) -> [usize; 6] {
        [
            self.gp.x[10],
            self.gp.x[11],
            self.gp.x[12],
            self.gp.x[13],
            self.gp.x[14],
            self.gp.x[15],
        ]
    }
    pub fn get_a0(&self) -> usize {
        self.gp.x[10]
    }

    pub fn get_reg(&self, index: usize) -> usize {
        self.gp.x[index]
    }

    pub fn set_reg(&mut self, index: usize, value: usize) {
        if index != 0 {
            self.gp.x[index] = value;
        }
    }

    pub fn set_a0(&mut self, val: usize) {
        self.gp.x[10] = val;
    }

    pub fn get_a1(&self) -> usize {
        self.gp.x[11]
    }

    pub fn set_a1(&mut self, val: usize) {
        self.gp.x[11] = val;
    }

    pub fn get_a2(&self) -> usize {
        self.gp.x[12]
    }

    pub fn set_a2(&mut self, val: usize) {
        self.gp.x[12] = val;
    }

    pub fn get_sepc(&self) -> usize {
        self.sepc
    }

    /// Return the raw saved status register value used when returning to user mode.
    pub fn get_status_bits(&self) -> usize {
        self.sstatus.bits()
    }

    pub fn set_sepc(&mut self, val: usize) {
        self.sepc = val
    }

    pub fn sepc_step(&mut self, val: isize) {
        self.sepc += val as usize
    }

    pub fn get_sp(&self) -> usize {
        self.gp.x[2]
    }

    pub fn set_sp(&mut self, sp: usize) {
        self.gp.x[2] = sp;
    }

    pub fn get_tp(&self) -> usize {
        self.gp.x[4]
    }

    pub fn set_tp(&mut self, tp: usize) {
        self.gp.x[4] = tp;
    }

    pub fn get_ra(&self) -> usize {
        self.gp.x[1]
    }

    pub fn set_ra(&mut self, ra: usize) {
        self.gp.x[1] = ra;
    }
}
