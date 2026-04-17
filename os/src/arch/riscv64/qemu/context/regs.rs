#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct GeneralRegs {
    pub x: [usize; 32],
}

/// FP registers
#[repr(C)]
#[derive(Debug, Default, Clone, Copy)]
pub struct FloatRegs {
    pub f: [usize; 32],
    pub fcsr: u32,
}
