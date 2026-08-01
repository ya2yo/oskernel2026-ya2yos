//! Lock-guarded public handle for a memory set.
//!
//! `MemorySet` wraps `MemorySetInner` in an `RwLock`. Process structures store
//! this type so callers do not hold the process metadata lock while touching
//! page tables or VM areas.

use alloc::{collections::BTreeMap, sync::Arc, vec::Vec};
use spin::rwlock::{RwLock, RwLockReadGuard, RwLockWriteGuard};

use super::{
    accessors::{writeback_shared_mmap_pages, SharedMmapWriteback},
    MemorySetInner,
};
#[cfg(feature = "fault-diagnostics")]
use crate::mm::map_area::MapType;
use crate::{
    arch::memory_layout::PAGE_SIZE,
    fs::{FilePage, FilePageCacheSource, FilePageKey, OSFile, FILE_PAGE_CACHE},
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

/// Read-only VMA and page-table state captured after an unrecoverable user
/// fault. This exists only in explicit diagnostic builds so normal fault paths
/// do not pay for VMA scanning or logging.
#[cfg(feature = "fault-diagnostics")]
#[derive(Debug, Clone, Copy)]
pub struct UserFaultDiagnostic {
    pub mapped_ppn: Option<usize>,
    pub pte_flags_bits: Option<usize>,
    pub pte_leaf_level: Option<usize>,
    pub pte_raw_bits: Option<usize>,
    pub pte_leaf_ppn: Option<usize>,
    pub page_table_token: usize,
    pub area: Option<UserFaultVma>,
}

#[cfg(feature = "fault-diagnostics")]
#[derive(Debug, Clone, Copy)]
pub struct UserFaultVma {
    pub start_vpn: usize,
    pub end_vpn: usize,
    pub area_type: MapAreaType,
    pub map_type: MapType,
    pub map_perm_bits: u8,
    pub mmap_flags_bits: usize,
    pub file_backed: bool,
    pub file_offset: usize,
    pub resident_frame: bool,
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
        // taking MemorySet's write lock. The page can legitimately bypass the
        // bounded global cache, so retain it until this fault installs it.
        let prepared = self.prepare_file_page(vpn);
        self.get_mut().handle_page_fault(vpn, scause, prepared)
    }

    /// Check whether a RISC-V leaf PTE already permits U-mode instruction
    /// fetch. A FetchInstructionPageFault in this state is retried once by
    /// the trap layer after local translation/instruction synchronization.
    #[cfg(target_arch = "riscv64")]
    #[inline(always)]
    pub fn is_user_executable(&self, vpn: VirtPageNum) -> bool {
        self.get_ref().page_table.is_user_executable(vpn)
    }

    /// Whether a faulting VPN lies in a file mapping beyond that file's EOF.
    ///
    /// The trap layer uses this to distinguish Linux SIGBUS from ordinary
    /// unmapped/protection faults, which result in SIGSEGV. Do not load the
    /// backing page here: an in-range fault immediately calls
    /// `handle_page_fault()`, which would otherwise duplicate the cache load.
    pub fn mmap_file_page_beyond_eof(&self, vpn: VirtPageNum) -> bool {
        let Some((inode, page_index)) = self.get_ref().mmap_file_page_info(vpn) else {
            return false;
        };
        let Some(file_offset) = page_index.checked_mul(PAGE_SIZE) else {
            return true;
        };
        file_offset >= inode.size()
    }

    /// Load one file-backed mmap page without holding the `MemorySet` lock.
    /// The returned `Arc` is handed to the immediately following installation
    /// step, including when the bounded global cache cannot retain it.
    fn prepare_file_page(&self, vpn: VirtPageNum) -> Option<Arc<FilePage>> {
        let request = self.get_ref().mmap_file_page_info(vpn);
        let (inode, page_index) = request?;
        FILE_PAGE_CACHE
            .get_or_load(inode, page_index, FilePageCacheSource::MmapDemand)
            .ok()
    }

    /// Preload all file pages in shared mappings before a fork takes the
    /// parent's MemorySet write lock to install shared frames.
    pub fn prefetch_shared_file_pages(&self) -> BTreeMap<FilePageKey, Arc<FilePage>> {
        let requests = self.get_ref().shared_file_page_info();
        let mut prepared = BTreeMap::new();
        for (inode, page_index) in requests {
            if let Ok(page) =
                FILE_PAGE_CACHE.get_or_load(inode, page_index, FilePageCacheSource::MmapPrefetch)
            {
                prepared.insert(page.key.clone(), page);
            }
        }
        prepared
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

    /// Capture VMA and resident-page state for an already failed user fault.
    /// The caller must discard this snapshot before signal delivery; it is not
    /// an API for normal page-fault decisions.
    #[cfg(feature = "fault-diagnostics")]
    pub fn fault_diagnostic(&self, vpn: VirtPageNum) -> UserFaultDiagnostic {
        let memory_set = self.get_ref();
        let mapped_ppn = memory_set.translate(vpn).map(|ppn| ppn.0);
        let pte_diagnostic = memory_set.page_table.translate_pte_diagnostic(vpn);
        let pte_flags_bits = pte_diagnostic.map(|(_, raw_bits, _)| raw_bits & 0x3ff);
        let pte_leaf_level = pte_diagnostic.map(|(level, _, _)| level);
        let pte_raw_bits = pte_diagnostic.map(|(_, raw_bits, _)| raw_bits);
        let pte_leaf_ppn = pte_diagnostic.map(|(_, _, ppn)| ppn);
        let page_table_token = memory_set.page_table.token();
        let area = memory_set
            .areas
            .iter()
            .find(|area| area.vpn_range.contains_vpn(vpn))
            .map(|area| {
                let (start, end) = area.vpn_range.range();
                UserFaultVma {
                    start_vpn: start.0,
                    end_vpn: end.0,
                    area_type: area.area_type,
                    map_type: area.map_type,
                    map_perm_bits: area.map_perm.bits(),
                    mmap_flags_bits: area.mmap_flags.bits() as usize,
                    file_backed: area.mmap_file.file.is_some(),
                    file_offset: area.mmap_file.offset,
                    resident_frame: area.data_frames.contains_key(&vpn),
                }
            });
        UserFaultDiagnostic {
            mapped_ppn,
            pte_flags_bits,
            pte_leaf_level,
            pte_raw_bits,
            pte_leaf_ppn,
            page_table_token,
            area,
        }
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
