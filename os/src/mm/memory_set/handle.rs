//! Lock-guarded public handle for a memory set.
//!
//! `MemorySet` wraps `MemorySetInner` in an `RwLock`. Process structures store
//! this type so callers do not hold the process metadata lock while touching
//! page tables or VM areas.

use alloc::{sync::Arc, vec::Vec};
use spin::rwlock::{RwLock, RwLockReadGuard, RwLockWriteGuard};

use super::{
    accessors::{writeback_shared_mmap_pages, SharedMmapWriteback},
    MemorySetInner,
};
use crate::{
    fs::{OSFile, FILE_PAGE_CACHE},
    mm::{
        FrameTracker, MapAreaType, MapPermission, PhysAddr, PhysPageNum, VPNRange, VirtAddr,
        VirtPageNum,
    },
    syscall::MmapFlags,
    trap::trap_types::Trap,
    utils::SyscallRet,
};

/// Thread-safe handle to a virtual address space.
pub struct MemorySet {
    inner: RwLock<MemorySetInner>,
}

impl MemorySet {
    /// Wrap an already constructed address-space object.
    pub fn new(memory_set: MemorySetInner) -> Self {
        Self {
            inner: RwLock::new(memory_set),
        }
    }

    /// Borrow the inner address space mutably.
    ///
    /// Keep this guard short-lived. Do not hold it across filesystem, network,
    /// futex, signal-delivery, or scheduler paths.
    pub fn get_mut(&self) -> RwLockWriteGuard<'_, MemorySetInner> {
        self.inner.write()
    }

    /// Borrow the inner address space read-only.
    pub fn get_ref(&self) -> RwLockReadGuard<'_, MemorySetInner> {
        self.inner.read()
    }

    /// Execute a closure while holding the write lock.
    pub fn with_mut<T>(&self, f: impl FnOnce(&mut MemorySetInner) -> T) -> T {
        let mut inner = self.get_mut();
        f(&mut inner)
    }

    /// Execute a closure while holding the read lock.
    pub fn with_ref<T>(&self, f: impl FnOnce(&MemorySetInner) -> T) -> T {
        let inner = self.get_ref();
        f(&inner)
    }

    /// Return the hardware page-table token.
    #[inline(always)]
    pub fn token(&self) -> usize {
        self.get_ref().token()
    }

    /// Eagerly map a framed logical area.
    #[inline(always)]
    pub fn insert_framed_area(
        &self,
        start_va: VirtAddr,
        end_va: VirtAddr,
        permission: MapPermission,
        area_type: MapAreaType,
    ) {
        self.get_mut()
            .insert_framed_area(start_va, end_va, permission, area_type)
    }

    /// Remove an area identified by its starting VPN.
    #[inline(always)]
    pub fn remove_area_with_start_vpn(&self, start_vpn: VirtPageNum) {
        self.get_mut().remove_area_with_start_vpn(start_vpn);
    }

    /// Create an anonymous/file-backed mmap area.
    #[inline(always)]
    pub fn mmap(
        &self,
        addr: usize,
        len: usize,
        map_perm: MapPermission,
        flags: MmapFlags,
        file: Option<Arc<OSFile>>,
        off: usize,
    ) -> usize {
        self.get_mut().mmap(addr, len, map_perm, flags, file, off)
    }

    /// Attach a SysV shared memory segment.
    #[inline(always)]
    pub fn shm(
        &self,
        addr: usize,
        size: usize,
        map_perm: MapPermission,
        pages: Vec<Arc<FrameTracker>>,
    ) -> usize {
        self.get_mut().shm(addr, size, map_perm, pages)
    }

    /// Detach a SysV shared memory segment from this address space.
    #[inline(always)]
    pub fn shm_detach(&self, addr: usize) -> SyscallRet {
        self.get_mut().shm_detach(addr)
    }

    /// Unmap an mmap-created range.
    #[inline(always)]
    pub fn munmap(&self, addr: usize, len: usize) -> SyscallRet {
        let end_addr = addr
            .checked_add(len)
            .ok_or(crate::utils::SysErrNo::EINVAL)?;
        let start_vpn = VirtAddr::from(addr).floor();
        let end_vpn = VirtAddr::from(end_addr).ceil();
        if start_vpn >= end_vpn {
            return Err(crate::utils::SysErrNo::EINVAL);
        }
        let writebacks: Vec<SharedMmapWriteback> = self
            .get_ref()
            .collect_shared_mmap_writebacks(Some((start_vpn, end_vpn)));
        for writeback in &writebacks {
            writeback_shared_mmap_pages(writeback)?;
        }
        self.get_mut().munmap(addr, len)
    }

    /// Handle a user page fault in this address space.
    #[inline(always)]
    pub fn handle_page_fault(&self, vpn: VirtPageNum, scause: Trap) -> bool {
        // File-backed faults may block in EXT4. Prepare the page before
        // taking MemorySet's write lock; the locked phase only installs the
        // already-cached frame into the VMA and page table.
        self.prepare_file_page(vpn);
        self.get_mut().handle_page_fault(vpn, scause)
    }

    /// Whether a faulting VPN lies in a file mapping beyond that file's EOF.
    ///
    /// The trap layer uses this to distinguish Linux SIGBUS from ordinary
    /// unmapped/protection faults, which result in SIGSEGV.
    pub fn mmap_file_page_beyond_eof(&self, vpn: VirtPageNum) -> bool {
        self.prepare_file_page(vpn).unwrap_or(false)
    }

    /// Load one file-backed mmap page without holding the MemorySet lock.
    /// Returns `Some(true)` when the page starts at or beyond EOF, `Some(false)`
    /// for a valid page, and `None` for non-file-backed mappings.
    fn prepare_file_page(&self, vpn: VirtPageNum) -> Option<bool> {
        let request = self.get_ref().mmap_file_page_info(vpn);
        let (inode, page_index) = request?;
        let page = FILE_PAGE_CACHE.get_or_load(inode, page_index).ok()?;
        Some(page.valid_len == 0)
    }

    /// Preload all file pages in shared mappings before a fork takes the
    /// parent's MemorySet write lock to install shared frames.
    pub fn prefetch_shared_file_pages(&self) {
        let requests = self.get_ref().shared_file_page_info();
        for (inode, page_index) in requests {
            let _ = FILE_PAGE_CACHE.get_or_load(inode, page_index);
        }
    }

    /// Change permissions for a virtual page range.
    ///
    /// This wrapper passes `if_mmap=false`, so file/offset metadata is left
    /// untouched. Area splitting and hardware PTE updates are implemented by
    /// `MemorySetInner::mprotect`.
    #[inline(always)]
    pub fn mprotect(&self, start_vpn: VirtPageNum, end_vpn: VirtPageNum, map_perm: MapPermission) {
        self.get_mut()
            .mprotect(start_vpn, end_vpn, map_perm, None, usize::MAX, None);
    }

    /// Activate this address space's page table on the current CPU.
    #[inline(always)]
    pub fn activate(&self) {
        self.get_ref().activate();
    }

    /// Drop all user VM areas and write back dirty shared mmap pages first.
    #[inline(always)]
    pub fn recycle_data_pages(&self) -> SyscallRet {
        let writebacks = self.get_ref().collect_shared_mmap_writebacks(None);
        let mut first_error = None;
        for writeback in &writebacks {
            if let Err(error) = writeback_shared_mmap_pages(writeback) {
                if first_error.is_none() {
                    first_error = Some(error);
                }
            }
        }
        let clear_result = self.get_mut().recycle_data_pages();
        match first_error {
            Some(error) => Err(error),
            None => clear_result,
        }
    }

    /// Resident physical memory in KiB.
    #[inline(always)]
    pub fn resident_size_kb(&self) -> usize {
        self.get_ref().resident_size_kb()
    }

    /// Locked virtual-memory size in KiB.
    #[inline(always)]
    pub fn locked_size_kb(&self) -> usize {
        self.get_ref().locked_size_kb()
    }

    /// Virtual address space size in KiB.
    #[inline(always)]
    pub fn virtual_size_kb(&self) -> usize {
        self.get_ref().virtual_size_kb()
    }

    /// Translate a virtual page number through the current page table.
    #[inline(always)]
    pub fn translate(&self, vpn: VirtPageNum) -> Option<PhysPageNum> {
        self.get_ref().translate(vpn)
    }

    /// Eagerly map a framed area below `hint`.
    ///
    /// Returns `(start_va, end_va)` for the inserted area.
    #[inline(always)]
    pub fn insert_framed_area_with_hint(
        &self,
        hint: usize,
        size: usize,
        map_perm: MapPermission,
        area_type: MapAreaType,
    ) -> (usize, usize) {
        self.get_mut()
            .insert_framed_area_with_hint(hint, size, map_perm, area_type)
    }

    /// Lazily map a framed area below `hint`.
    ///
    /// Pages are allocated on first page fault. Returns `(start_va, end_va)`.
    #[inline(always)]
    pub fn lazy_insert_framed_area_with_hint(
        &self,
        hint: usize,
        size: usize,
        map_perm: MapPermission,
        area_type: MapAreaType,
    ) -> (usize, usize) {
        self.get_mut()
            .lazy_insert_framed_area_with_hint(hint, size, map_perm, area_type)
    }

    /// Copy a lazily allocated logical area from another address space.
    #[inline(always)]
    pub fn lazy_clone_area(&self, start_vpn: VirtPageNum, another: &MemorySetInner) {
        self.get_mut().lazy_clone_area(start_vpn, another)
    }

    /// Translate a virtual address to a physical address if already mapped.
    ///
    /// This does not trigger lazy allocation. Use user-copy helpers when a
    /// faultable user pointer should be handled gracefully.
    pub fn translate_va(&self, va: VirtAddr) -> Option<PhysAddr> {
        self.get_ref().page_table.translate_va(va)
    }

    /// Check that a user byte range is fully covered by areas with permissions.
    pub fn check_user_range(&self, start: usize, len: usize, wanted_perm: MapPermission) -> bool {
        if len == 0 {
            return true;
        }

        let end = match start.checked_add(len) {
            Some(v) => v,
            None => return false,
        };

        let start_vpn = VirtAddr::from(start).floor();
        let end_vpn = VirtAddr::from(end - 1).ceil();

        self.get_ref()
            .check_user_range(VPNRange::new(start_vpn, end_vpn), wanted_perm)
    }
}
