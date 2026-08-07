use crate::{mm::uaccess::Scope, mm::MemorySet};

extern "C" {
    fn __ya2y_raw_copy_from_user(src: usize, dst: *mut u8, len: usize) -> usize;
    fn __ya2y_raw_copy_to_user(src: *const u8, dst: usize, len: usize) -> usize;
    fn __ya2y_uaccess_fault_fixup();
}

struct SupervisorUserAccess {
    restore_sum: bool,
}

impl SupervisorUserAccess {
    #[inline]
    fn enable() -> Self {
        use riscv::register::sstatus;

        let restore_sum = !sstatus::read().sum();
        if restore_sum {
            unsafe { sstatus::set_sum() };
        }
        Self { restore_sum }
    }
}

impl Drop for SupervisorUserAccess {
    #[inline]
    fn drop(&mut self) {
        if self.restore_sum {
            unsafe { riscv::register::sstatus::clear_sum() };
        }
    }
}

#[inline(never)]
pub(crate) fn copy_from_user(memory_set: &MemorySet, src: usize, dst: &mut [u8]) -> bool {
    let _scope = Scope::enter(memory_set, __ya2y_uaccess_fault_fixup as *const () as usize);
    let _sum = SupervisorUserAccess::enable();
    unsafe { __ya2y_raw_copy_from_user(src, dst.as_mut_ptr(), dst.len()) == 0 }
}

#[inline(never)]
pub(crate) fn copy_to_user(memory_set: &MemorySet, src: &[u8], dst: usize) -> bool {
    let _scope = Scope::enter(memory_set, __ya2y_uaccess_fault_fixup as *const () as usize);
    let _sum = SupervisorUserAccess::enable();
    unsafe { __ya2y_raw_copy_to_user(src.as_ptr(), dst, src.len()) == 0 }
}
