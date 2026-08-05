//! Process and kernel virtual-address spaces.
//!
//! This module keeps the public `MemorySet` API in one place while splitting
//! the implementation by responsibility:
//!
//! - [`types`]: unlocked address-space state (`MemorySetInner`);
//! - [`handle`]: lock-guarded public handle (`MemorySet`);
//! - [`area_ops`]: common `MapArea` insertion, removal, growth and clone helpers;
//! - [`accessors`]: page-table access, memory accounting and teardown;
//! - [`elf_loader`]: ELF program and dynamic-linker loading;
//! - [`fork_clone`]: fork/clone address-space duplication;
//! - [`kernel_init`]: kernel address-space construction;
//! - [`mmap_ops`]: mmap/munmap/mprotect and shared memory attach/detach;
//! - [`pagefault`]: user-space page-fault handling.

mod accessors;
mod area_ops;
mod elf_loader;
mod fork_clone;
mod handle;
mod kernel_init;
mod mmap_ops;
mod pagefault;
mod types;

use crate::mm::{
    read_user_bytes_direct_into, user_buffer_from_kernel, FrameTracker, MapArea, MapAreaType,
    MapPermission, PhysAddr, UserBuffer, VPNRange, VirtAddr, VirtPageNum,
};
use spin::{Lazy, Mutex};

pub(crate) use elf_loader::read_elf_metadata_with_prefix;
pub use elf_loader::*;
pub use fork_clone::*;
pub use handle::*;
pub use kernel_init::*;
pub use mmap_ops::*;
pub use types::*;

/// Kernel virtual-address space.
///
/// The kernel space is initialized lazily during `mm::init()`. User address
/// spaces are created with kernel mappings copied in, so this remains the
/// source of truth for kernel page-table layout.
pub static KERNEL_SPACE: Lazy<Mutex<MemorySetInner>> =
    Lazy::new(|| Mutex::new(MemorySetInner::new_kernel()));

#[allow(unused)]
#[cfg(target_arch = "riscv64")]
/// Run the architecture page-table remap sanity check.
pub fn remap_test() {
    kernel_init::remap_test();
}

#[allow(unused)]
#[cfg(target_arch = "loongarch64")]
/// Run the architecture page-table remap sanity check.
pub fn remap_test() {
    kernel_init::remap_test();
}
