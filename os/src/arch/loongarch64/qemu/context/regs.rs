use core::fmt::Debug;

/// General registers
#[repr(C)]
#[derive(Default, Clone, Copy)]
pub struct GeneralRegs {
    pub pc: usize, // 这里保存的是异常返回地址，也就是异常发生时的PC（理解为sepc）
    pub ra: usize,
    pub tp: usize,
    pub sp: usize,
    pub a0: usize,
    pub a1: usize,
    pub a2: usize,
    pub a3: usize,
    pub a4: usize,
    pub a5: usize,
    pub a6: usize,
    pub a7: usize,
    pub t0: usize,
    pub t1: usize,
    pub t2: usize,
    pub t3: usize,
    pub t4: usize,
    pub t5: usize,
    pub t6: usize,
    pub t7: usize,
    pub t8: usize,
    pub r21: usize,
    pub fp: usize,
    pub s0: usize,
    pub s1: usize,
    pub s2: usize,
    pub s3: usize,
    pub s4: usize,
    pub s5: usize,
    pub s6: usize,
    pub s7: usize,
    pub s8: usize,
}

/// FP registers
#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct FloatRegs {
    pub f: [usize; 32],
    pub fcsr: u32, // 浮点控制状态寄存器
    pub fcc: u8,   // 浮点条件标志寄存器集合（一共有8个，每个标志寄存器只需1bit）
}

impl Debug for GeneralRegs {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("GeneralRegs")
            .field("pc", &format_args!("{:#x}", self.pc))
            .field("ra", &format_args!("{:#x}", self.ra))
            .field("tp", &format_args!("{:#x}", self.tp))
            .field("sp", &format_args!("{:#x}", self.sp))
            .field("a0", &self.a0)
            .field("a1", &self.a1)
            .field("a2", &self.a2)
            .field("a3", &self.a3)
            .field("a4", &self.a4)
            .field("a5", &self.a5)
            .field("a6", &self.a6)
            .field("a7", &self.a7)
            .field("t0", &self.t0)
            .field("t1", &self.t1)
            .field("t2", &self.t2)
            .field("t3", &self.t3)
            .field("t4", &self.t4)
            .field("t5", &self.t5)
            .field("t6", &self.t6)
            .field("t7", &self.t7)
            .field("t8", &self.t8)
            .field("r21", &self.r21)
            .field("fp", &format_args!("{:#x}", self.fp))
            .field("s0", &self.s0)
            .field("s1", &self.s1)
            .field("s2", &self.s2)
            .field("s3", &self.s3)
            .field("s4", &self.s4)
            .field("s5", &self.s5)
            .field("s6", &self.s6)
            .field("s7", &self.s7)
            .field("s8", &self.s8)
            .finish()
    }
}
