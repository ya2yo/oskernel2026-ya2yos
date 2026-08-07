//! Lock-guarded public handle for a memory set.
//!
//! `MemorySet` wraps `MemorySetInner` in an `RwLock`. Process structures store
//! this type so callers do not hold the process metadata lock while touching
//! page tables or VM areas.

use alloc::{collections::BTreeMap, sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicUsize, Ordering};
use spin::rwlock::{RwLock, RwLockReadGuard};

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
    /// Harts which may still access this page table or its user frames.
    ///
    /// A bit is installed while holding `inner`'s read lock immediately before
    /// returning to user mode, and remains set through user traps until the
    /// scheduler detaches the task.  Keeping it set in kernel mode also covers
    /// direct `copy_{from,to}_user` accesses during a syscall.
    active_harts: AtomicUsize,
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
            active_harts: AtomicUsize::new(0),
        }
    }

    /// Borrow the inner address space read-only.
    ///
    /// A page-table writer holds this lock while waiting for remote TLB
    /// acknowledgements. A reader can be one of those remote harts while it
    /// returns from a syscall with interrupts disabled, so service the
    /// mailbox between failed lock attempts instead of spinning forever.
    pub(crate) fn get_ref(&self) -> RwLockReadGuard<'_, MemorySetInner> {
        loop {
            if let Some(guard) = self.inner.try_read() {
                return guard;
            }
            crate::mm::remote_tlb::poll();
            core::hint::spin_loop();
        }
    }

    /// Execute a page-table update with the remote TLB protocol in place.
    ///
    /// The retained frame list deliberately covers the full resident set, not
    /// just a best-effort list of pages touched by the caller.  This keeps
    /// unmapped or COW-replaced frames alive until all remote stale TLB entries
    /// have been invalidated.
    pub(crate) fn with_mut<T>(&self, f: impl FnOnce(&mut MemorySetInner) -> T) -> T {
        let was_active = self.deactivate_current_hart();
        let _update_guard = crate::mm::remote_tlb::lock_updates();
        let mut inner = self.inner.write();
        let retained_frames: Vec<Arc<FrameTracker>> = inner
            .areas
            .iter()
            .flat_map(|area| area.data_frames.values().cloned())
            .collect();
        let result = f(&mut inner);
        crate::mm::remote_tlb::shootdown(&self.active_harts);
        if was_active {
            self.activate_current_hart();
        }
        drop(retained_frames);
        result
    }

    /// Update page-table state without retiring or replacing resident frames.
    ///
    /// Permission-only updates still need the normal remote TLB protocol, but
    /// cloning the full resident set is unnecessary when every frame remains
    /// owned by the address space throughout the update.
    fn with_frame_preserving_mut<T>(&self, f: impl FnOnce(&mut MemorySetInner) -> T) -> T {
        let was_active = self.deactivate_current_hart();
        let _update_guard = crate::mm::remote_tlb::lock_updates();
        let mut inner = self.inner.write();
        let result = f(&mut inner);
        crate::mm::remote_tlb::shootdown(&self.active_harts);
        if was_active {
            self.activate_current_hart();
        }
        result
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
        self.with_mut(|inner| inner.insert_framed_area(start_va, end_va, permission, area_type))
    }

    /// Remove an area identified by its starting VPN.
    #[inline(always)]
    pub fn remove_area_with_start_vpn(&self, start_vpn: VirtPageNum) {
        self.with_mut(|inner| inner.remove_area_with_start_vpn(start_vpn));
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
        if flags.contains(MmapFlags::MAP_FIXED) {
            self.with_mut(|inner| inner.mmap(addr, len, map_perm, flags, file, off))
        } else {
            self.with_vma_mut(|inner| inner.mmap(addr, len, map_perm, flags, file, off))
        }
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
        self.with_mut(|inner| inner.shm(addr, size, map_perm, pages))
    }

    /// Detach a SysV shared memory segment from this address space.
    #[inline(always)]
    pub fn shm_detach(&self, addr: usize) -> SyscallRet {
        self.with_mut(|inner| inner.shm_detach(addr))
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
        self.with_mut(|inner| inner.munmap(addr, len))
    }

    /// Validate that a memory-advice range is fully mapped.
    #[inline]
    pub fn validate_madvise_range(&self, addr: usize, len: usize) -> SyscallRet {
        self.get_ref().validate_madvise_range(addr, len)
    }

    /// Discard resident pages in a mapped range and leave the VMAs intact.
    #[inline]
    pub fn discard_madvise_pages(&self, addr: usize, len: usize) -> SyscallRet {
        self.with_mut(|inner| inner.discard_madvise_pages(addr, len))
    }

    /// Handle a user page fault in this address space.
    #[inline(always)]
    pub fn handle_page_fault(&self, vpn: VirtPageNum, scause: Trap) -> bool {
        // File-backed faults may block in EXT4. Prepare the page before
        // taking MemorySet's write lock. The page can legitimately bypass the
        // bounded global cache, so retain it until this fault installs it.
        let prepared = self.prepare_file_page(vpn);

        // A first demand mapping cannot leave a valid remote translation for
        // this VPN, so it needs no IPI broadcast.  A present PTE may be a COW
        // or permission-protected mapping; retain its frames and flush every
        // active hart before allowing the old mapping to be reclaimed.
        let was_active = self.deactivate_current_hart();
        let _update_guard = crate::mm::remote_tlb::lock_updates();
        let mut inner = self.inner.write();
        let replaces_present_pte = inner.page_table.translate(vpn).is_some();
        let retained_frames: Vec<Arc<FrameTracker>> = replaces_present_pte
            .then(|| {
                inner
                    .areas
                    .iter()
                    .flat_map(|area| area.data_frames.values().cloned())
                    .collect()
            })
            .unwrap_or_default();
        let handled = inner.handle_page_fault(vpn, scause, prepared);
        if replaces_present_pte {
            crate::mm::remote_tlb::shootdown(&self.active_harts);
        }
        if was_active {
            self.activate_current_hart();
        }
        drop(retained_frames);
        handled
    }

    /// Check whether a leaf PTE already permits U-mode instruction fetch.
    /// A present instruction/load fault in this state is retried once by the
    /// trap layer after local translation synchronization.
    #[cfg(any(target_arch = "riscv64", target_arch = "loongarch64"))]
    #[inline(always)]
    pub fn is_user_executable(&self, vpn: VirtPageNum) -> bool {
        self.get_ref().page_table.is_user_executable(vpn)
    }

    /// Check whether a present leaf PTE permits a U-mode load.
    #[cfg(any(target_arch = "riscv64", target_arch = "loongarch64"))]
    #[inline(always)]
    pub fn is_user_readable(&self, vpn: VirtPageNum) -> bool {
        self.get_ref().page_table.is_user_readable(vpn)
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
        self.with_frame_preserving_mut(|inner| inner.mprotect(start_vpn, end_vpn, map_perm));
    }

    /// Activate this address space for a user-mode return on the current CPU.
    ///
    /// The active bit is published before the read lock is released.  A page
    /// table writer therefore either sees this hart in its shootdown mask or
    /// completes before this hart installs the page table locally.
    #[inline(always)]
    pub fn activate_for_user(&self) {
        let inner = self.get_ref();
        inner.activate();
        self.activate_current_hart();
    }

    /// Install this page table without publishing a user-mode active bit.
    ///
    /// `execve` uses this while replacing the process resource slot: the old
    /// address space remains active until that swap clears its bit, and the
    /// final `trap_return()` publishes the new one.
    #[inline(always)]
    pub(crate) fn activate(&self) {
        self.get_ref().activate();
    }

    /// Mark the current hart as no longer executing this address space.
    #[inline(always)]
    pub(crate) fn deactivate_current_hart(&self) -> bool {
        let hart = crate::arch::cpu::hart_id();
        let bit = 1usize << hart;
        self.active_harts.fetch_and(!bit, Ordering::AcqRel) & bit != 0
    }

    #[inline(always)]
    fn activate_current_hart(&self) {
        let hart = crate::arch::cpu::hart_id();
        self.active_harts
            .fetch_or(1usize << hart, Ordering::Release);
    }

    /// Update only VMA metadata without changing an existing PTE.
    ///
    /// Non-fixed `mmap` and `MAP_FIXED_NOREPLACE` first establish that the
    /// destination does not overlap an existing VMA, then append a lazy VMA.
    /// They therefore cannot leave a stale valid translation on another hart
    /// and do not need frame retention or a remote TLB shootdown.
    fn with_vma_mut<T>(&self, f: impl FnOnce(&mut MemorySetInner) -> T) -> T {
        let was_active = self.deactivate_current_hart();
        let mut inner = self.inner.write();
        let result = f(&mut inner);
        if was_active {
            self.activate_current_hart();
        }
        result
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
        let clear_result = self.with_mut(|inner| inner.recycle_data_pages());
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
        self.with_mut(|inner| inner.insert_framed_area_with_hint(hint, size, map_perm, area_type))
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
        self.with_mut(|inner| {
            inner.lazy_insert_framed_area_with_hint(hint, size, map_perm, area_type)
        })
    }

    /// Copy a lazily allocated logical area from another address space.
    #[inline(always)]
    pub fn lazy_clone_area(&self, start_vpn: VirtPageNum, another: &MemorySetInner) {
        self.with_mut(|inner| inner.lazy_clone_area(start_vpn, another))
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
