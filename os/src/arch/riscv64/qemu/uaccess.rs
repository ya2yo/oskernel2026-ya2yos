//! RISC-V 用户地址空间访问的架构相关快速路径。
//!
//! 本模块只负责在当前地址空间已经激活时，直接通过用户虚拟地址进行
//! 小块数据复制。通用的地址范围检查、权限检查、跨页处理以及缺页回退
//! 由 [`crate::mm::copy_from_user`] 和 [`crate::mm::copy_to_user`] 完成。
//!
//! 直接访问用户页前需要同时建立 [`Scope`]，以便内核异常入口识别并恢复
//! 用户访问引起的页错误，以及临时打开 RISC-V `sstatus.SUM` 位。底层
//! 汇编复制失败时通过 fixup 标签返回，调用方随后会转入软件页表复制路径。

use crate::{mm::uaccess::Scope, mm::MemorySet};

extern "C" {
    /// 从用户虚拟地址 `src` 复制 `len` 字节到内核缓冲区 `dst`。
    ///
    /// 返回值为 0 表示复制完成；非零值表示访问用户地址时发生异常，
    /// 并经由 [`__ya2y_uaccess_fault_fixup`] 返回。
    fn __ya2y_raw_copy_from_user(src: usize, dst: *mut u8, len: usize) -> usize;
    /// 从内核缓冲区 `src` 复制 `len` 字节到用户虚拟地址 `dst`。
    ///
    /// 返回值语义与 [`__ya2y_raw_copy_from_user`] 相同。
    fn __ya2y_raw_copy_to_user(src: *const u8, dst: usize, len: usize) -> usize;
    /// 用户访问发生不可恢复异常时使用的汇编 fixup 返回路径。
    fn __ya2y_uaccess_fault_fixup();
}

/// 管理一次临时的 RISC-V Supervisor 对用户页访问权限。
///
/// `SUM` 原本的状态会在创建 guard 时保存，并在 guard 离开作用域时
/// 自动恢复，避免用户访问状态泄漏到后续内核代码。
struct SupervisorUserAccess {
    restore_sum: bool,
}

impl SupervisorUserAccess {
    /// 打开 `SUM`，并记录是否需要在离开作用域时恢复它。
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
    /// 恢复进入本次复制前的 `SUM` 状态。
    #[inline]
    fn drop(&mut self) {
        if self.restore_sum {
            unsafe { riscv::register::sstatus::clear_sum() };
        }
    }
}

/// 在当前已激活的用户地址空间中，直接将用户数据复制到内核缓冲区。
///
/// 这是 [`crate::mm::copy_from_user`] 使用的 RISC-V 快速路径，不应绕过
/// mm 层直接作为通用用户指针检查接口。调用者必须保证 `memory_set` 是
/// 当前任务正在使用的地址空间；复制失败时返回 `false`，由上层转入
/// 软件页表复制路径。
#[inline(never)]
pub(crate) fn copy_from_user(memory_set: &MemorySet, src: usize, dst: &mut [u8]) -> bool {
    let _scope = Scope::enter(memory_set, __ya2y_uaccess_fault_fixup as *const () as usize);
    let _sum = SupervisorUserAccess::enable();
    unsafe { __ya2y_raw_copy_from_user(src, dst.as_mut_ptr(), dst.len()) == 0 }
}

/// 在当前已激活的用户地址空间中，直接将内核数据复制到用户缓冲区。
///
/// 这是 [`crate::mm::copy_to_user`] 使用的 RISC-V 快速路径。函数建立
/// uaccess fault 作用域并临时打开 `SUM`；如果底层访问无法完成，则返回
/// `false`，由上层使用软件页表路径处理缺页或其他不可恢复错误。
#[inline(never)]
pub(crate) fn copy_to_user(memory_set: &MemorySet, src: &[u8], dst: usize) -> bool {
    let _scope = Scope::enter(memory_set, __ya2y_uaccess_fault_fixup as *const () as usize);
    let _sum = SupervisorUserAccess::enable();
    unsafe { __ya2y_raw_copy_to_user(src.as_ptr(), dst, src.len()) == 0 }
}
