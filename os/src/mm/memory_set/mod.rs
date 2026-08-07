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
//!
//! # Lock ordering
//!
//! Page-table mutation follows this order:
//!
//! 1. `remote_tlb::UPDATE_LOCK`;
//! 2. exactly one `MemorySet::inner` write lock;
//! 3. MM child locks reached by the operation, such as `GROUP_SHARE`, frame
//!    allocation, or page-cache locks.
//!
//! Never acquire `UPDATE_LOCK` while a `MemorySet` guard is held, and never
//! hold two `MemorySet` guards at once. Cross-address-space operations must
//! snapshot `Arc`-owned frames or scalar metadata from the source, release its
//! guard, and only then lock the destination. Do not enter filesystem, network,
//! scheduler, futex, or signal-delivery while holding a `MemorySet` guard.
//! The only user-memory exception is the bounded current-address-space fast
//! path: it verifies every PTE before entering its architecture-specific
//! uaccess mode (RISC-V SUM) and must not fault.

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
