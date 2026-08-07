//! Core address-space data structures.
//!
//! `MemorySetInner` is the unlocked address-space object: it owns the page
//! table and the ordered list of logical VM areas. Public users should normally
//! hold an `Arc<MemorySet>` and go through `MemorySet`'s lock-guarded methods.

use alloc::vec::Vec;

use crate::{arch::page_table::PageTable, mm::MapArea};

/// A process/kernel virtual address space.
///
/// Each address space consists of:
/// - one hardware page table;
/// - a list of logical mapping areas (`MapArea`);
/// - mmap accounting used to cap lazy user mappings.
pub struct MemorySetInner {
    /// Page table used by hardware address translation.
    pub page_table: PageTable,
    /// Logical VM areas. Each area records permissions, mapping type, backing
    /// frames or file metadata, and the virtual page range it covers.
    pub areas: Vec<MapArea>,
    /// Total virtual memory allocated via mmap, in bytes.
    ///
    /// This is accounting for mmap-created VMAs only. It is used to avoid
    /// runaway lazy mmap allocation exhausting physical memory later through
    /// page faults.
    pub total_mmap_size: usize,
    /// Top-down allocation cursor for non-fixed mmap-like mappings.
    ///
    /// The cursor is only a hint: fixed mappings and holes can invalidate it,
    /// so callers must still use the ordered VMA search as a fallback.
    pub(crate) mmap_hint: usize,
}
