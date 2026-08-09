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

    /// Execute a page-table update while retaining the frames selected before
    /// the update until every remote stale translation has been invalidated.
    fn with_retained_frames_mut<T>(
        &self,
        #[cfg(feature = "perf")] kind: crate::mm::remote_tlb::ShootdownKind,
        retain: impl FnOnce(&MemorySetInner) -> Vec<Arc<FrameTracker>>,
        f: impl FnOnce(&mut MemorySetInner) -> T,
    ) -> T {
        let was_active = self.deactivate_current_hart();
        let _update_guard = crate::mm::remote_tlb::lock_updates();
        let mut inner = self.inner.write();
        let retained_frames = retain(&inner);
        let result = f(&mut inner);
        crate::mm::remote_tlb::shootdown(
            &self.active_harts,
            #[cfg(feature = "perf")]
            kind,
        );
        if was_active {
            self.activate_current_hart();
        }
        drop(retained_frames);
        result
    }

    /// Execute a broad page-table update which may retire any resident frame.
    /// Prefer a range-targeted or frame-preserving entry for bounded updates.
    pub(crate) fn with_mut<T>(&self, f: impl FnOnce(&mut MemorySetInner) -> T) -> T {
        self.with_retained_frames_mut(
            #[cfg(feature = "perf")]
            crate::mm::remote_tlb::ShootdownKind::Other,
            |inner| {
                inner
                    .areas
                    .iter()
                    .flat_map(|area| area.data_frames.values().cloned())
                    .collect()
            },
            f,
        )
    }

    /// Execute an update which can retire frames only inside `ranges`.
    fn with_ranges_mut<T>(
        &self,
        #[cfg(feature = "perf")] kind: crate::mm::remote_tlb::ShootdownKind,
        ranges: &[(VirtPageNum, VirtPageNum)],
        f: impl FnOnce(&mut MemorySetInner) -> T,
    ) -> T {
        self.with_retained_frames_mut(
            #[cfg(feature = "perf")]
            kind,
            |inner| {
                let mut retained = Vec::new();
                for area in &inner.areas {
                    let (area_start, area_end) = area.vpn_range.range();
                    for &(start, end) in ranges {
                        if start >= end || area_start >= end || start >= area_end {
                            continue;
                        }
                        retained.extend(
                            area.data_frames
                                .range(start..end)
                                .map(|(_, frame)| Arc::clone(frame)),
                        );
                    }
                }
                retained
            },
            f,
        )
    }

    #[inline]
    fn with_range_mut<T>(
        &self,
        #[cfg(feature = "perf")] kind: crate::mm::remote_tlb::ShootdownKind,
        start: VirtPageNum,
        end: VirtPageNum,
        f: impl FnOnce(&mut MemorySetInner) -> T,
    ) -> T {
        self.with_ranges_mut(
            #[cfg(feature = "perf")]
            kind,
            &[(start, end)],
            f,
        )
    }

    /// Update page-table state without retiring or replacing resident frames.
    ///
    /// Permission-only updates still need the normal remote TLB protocol, but
    /// cloning the full resident set is unnecessary when every frame remains
    /// owned by the address space throughout the update.
    pub(crate) fn with_frame_preserving_mut<T>(
        &self,
        f: impl FnOnce(&mut MemorySetInner) -> T,
    ) -> T {
        self.with_frame_preserving_mut_with_kind(
            #[cfg(feature = "perf")]
            crate::mm::remote_tlb::ShootdownKind::Other,
            f,
        )
    }

    /// Update page-table state without retiring resident frames, preserving
    /// the source category in remote-TLB diagnostics.
    pub(crate) fn with_frame_preserving_mut_with_kind<T>(
        &self,
        #[cfg(feature = "perf")] kind: crate::mm::remote_tlb::ShootdownKind,
        f: impl FnOnce(&mut MemorySetInner) -> T,
    ) -> T {
        self.with_retained_frames_mut(
            #[cfg(feature = "perf")]
            kind,
            |_| Vec::new(),
            f,
        )
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
        self.with_frame_preserving_mut(|inner| {
            inner.insert_framed_area(start_va, end_va, permission, area_type)
        })
    }

    /// Remove an area identified by its starting VPN.
    #[inline(always)]
    pub fn remove_area_with_start_vpn(&self, start_vpn: VirtPageNum) {
        self.with_retained_frames_mut(
            #[cfg(feature = "perf")]
            crate::mm::remote_tlb::ShootdownKind::Other,
            |inner| {
                inner
                    .areas
                    .iter()
                    .find(|area| area.vpn_range.start() == start_vpn)
                    .map(|area| area.data_frames.values().cloned().collect())
                    .unwrap_or_default()
            },
            |inner| inner.remove_area_with_start_vpn(start_vpn),
        );
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
            let Some(end_addr) = addr.checked_add(len) else {
                return 0;
            };
            self.with_range_mut(
                #[cfg(feature = "perf")]
                crate::mm::remote_tlb::ShootdownKind::Other,
                VirtAddr::from(addr).floor(),
                VirtAddr::from(end_addr).ceil(),
                |inner| inner.mmap(addr, len, map_perm, flags, file, off),
            )
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
        self.with_frame_preserving_mut(|inner| inner.shm(addr, size, map_perm, pages))
    }

    /// Detach a SysV shared memory segment from this address space.
    #[inline(always)]
    pub fn shm_detach(&self, addr: usize) -> SyscallRet {
        let start_vpn = VirtAddr::from(addr).floor();
        self.with_retained_frames_mut(
            #[cfg(feature = "perf")]
            crate::mm::remote_tlb::ShootdownKind::Other,
            |inner| {
                inner
                    .areas
                    .iter()
                    .find(|area| area.vpn_range.start() == start_vpn)
                    .map(|area| area.data_frames.values().cloned().collect())
                    .unwrap_or_default()
            },
            |inner| inner.shm_detach(addr),
        )
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
        self.with_range_mut(
            #[cfg(feature = "perf")]
            crate::mm::remote_tlb::ShootdownKind::Munmap,
            start_vpn,
            end_vpn,
            |inner| inner.munmap(addr, len),
        )
    }

    /// Validate that a memory-advice range is fully mapped.
    #[inline]
    pub fn validate_madvise_range(&self, addr: usize, len: usize) -> SyscallRet {
        self.get_ref().validate_madvise_range(addr, len)
    }

    /// Discard resident pages in a mapped range and leave the VMAs intact.
    #[inline]
    pub fn discard_madvise_pages(&self, addr: usize, len: usize) -> SyscallRet {
        let end_addr = addr
            .checked_add(len)
            .ok_or(crate::utils::SysErrNo::EINVAL)?;
        self.with_range_mut(
            #[cfg(feature = "perf")]
            crate::mm::remote_tlb::ShootdownKind::Other,
            VirtAddr::from(addr).floor(),
            VirtAddr::from(end_addr).ceil(),
            |inner| inner.discard_madvise_pages(addr, len),
        )
    }

    /// Handle a user page fault in this address space.
    #[inline(always)]
    pub fn handle_page_fault(&self, vpn: VirtPageNum, scause: Trap) -> bool {
        // File-backed faults may block in EXT4. Prepare the page before
        // taking MemorySet's write lock. The page can legitimately bypass the
        // bounded global cache, so retain it until this fault installs it.
        let prepared = self.prepare_file_page(vpn);

        let was_active = self.deactivate_current_hart();

        // Fast path: the faulting VPN has no present PTE, so this is a fresh
        // demand mapping that cannot leave a stale remote translation behind.
        // It needs neither the global UPDATE_LOCK nor a shootdown broadcast;
        // the translate check and the install run atomically under the
        // address-space write lock, so no other writer can slip a present PTE
        // in between them.
        {
            let mut inner = self.inner.write();
            if inner.page_table.translate(vpn).is_none() {
                let handled = inner.handle_page_fault(vpn, scause, prepared);
                if was_active {
                    self.activate_current_hart();
                }
                return handled;
            }
        }

        // A present page can require only a local permission/dirty-bit update.
        // Do that under the address-space lock alone: its PPN is unchanged, so
        // stale remote entries are conservatively more restrictive and will
        // fault/reload locally before a remote store can proceed.  In
        // particular, decide whether a COW page is shared before cloning its
        // frame below; the clone is only for a real PPN replacement and would
        // otherwise turn a refcount-one page into a forced COW copy.
        {
            let mut inner = self.inner.write();
            let cow_copy = inner.cow_fault_requires_frame_copy(vpn, scause);
            if cow_copy != Some(true) {
                let handled = inner.handle_page_fault(vpn, scause, prepared);
                #[cfg(feature = "perf")]
                if handled {
                    if let Some(requires_copy) = cow_copy {
                        crate::utils::perf::record_cow_fault_resolution(requires_copy);
                    }
                }
                if was_active {
                    self.activate_current_hart();
                }
                return handled;
            }
        }

        // A shared COW page is the one present-fault case which replaces a
        // PPN. Serialize the sender, pin the old frame, then wait for every
        // active remote hart before that pin is released. Lock order remains
        // UPDATE_LOCK -> MemorySet write lock as required by all replacement
        // paths. Re-check after reacquiring `inner`: another writer may have
        // resolved this COW fault while this hart waited for UPDATE_LOCK.
        let _update_guard = crate::mm::remote_tlb::lock_updates();
        let mut inner = self.inner.write();
        let cow_copy = inner.cow_fault_requires_frame_copy(vpn, scause);
        let retained_frames: Vec<Arc<FrameTracker>> = (cow_copy == Some(true))
            .then(|| {
                inner
                    .areas
                    .iter()
                    .filter_map(|area| area.data_frames.get(&vpn).cloned())
                    .collect()
            })
            .unwrap_or_default();
        let handled = inner.handle_page_fault(vpn, scause, prepared);
        #[cfg(feature = "perf")]
        if handled {
            if let Some(requires_copy) = cow_copy {
                crate::utils::perf::record_cow_fault_resolution(requires_copy);
            }
        }
        if handled && cow_copy == Some(true) {
            crate::mm::remote_tlb::shootdown(
                &self.active_harts,
                #[cfg(feature = "perf")]
                crate::mm::remote_tlb::ShootdownKind::Cow,
            );
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

    /// Whether a kernel-originated load may access a present user leaf.
    #[cfg(target_arch = "riscv64")]
    #[inline(always)]
    pub(crate) fn is_kernel_user_readable(&self, vpn: VirtPageNum) -> bool {
        self.get_ref().page_table.is_user_readable(vpn)
    }

    /// Whether a kernel-originated load may access a present user leaf.
    #[cfg(target_arch = "loongarch64")]
    #[inline(always)]
    pub(crate) fn is_kernel_user_readable(&self, vpn: VirtPageNum) -> bool {
        self.get_ref().page_table.is_kernel_user_readable(vpn)
    }

    /// Whether a kernel-originated store may access a present user leaf.
    #[cfg(target_arch = "riscv64")]
    #[inline(always)]
    pub(crate) fn is_kernel_user_writable(&self, vpn: VirtPageNum) -> bool {
        self.get_ref().page_table.is_user_writable(vpn)
    }

    /// Whether a kernel-originated store may access a present user leaf.
    #[cfg(target_arch = "loongarch64")]
    #[inline(always)]
    pub(crate) fn is_kernel_user_writable(&self, vpn: VirtPageNum) -> bool {
        self.get_ref().page_table.is_kernel_user_writable(vpn)
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
        self.with_frame_preserving_mut_with_kind(
            #[cfg(feature = "perf")]
            crate::mm::remote_tlb::ShootdownKind::Mprotect,
            |inner| inner.mprotect(start_vpn, end_vpn, map_perm),
        );
    }

    /// Adjust an mmap range while retaining only frames which the operation can
    /// remove from the old or fixed destination ranges.
    pub fn mremap(
        &self,
        old_addr: usize,
        old_len: usize,
        new_len: usize,
        new_addr: usize,
        may_move: bool,
        fixed: bool,
    ) -> SyscallRet {
        let old_end = old_addr
            .checked_add(old_len)
            .ok_or(crate::utils::SysErrNo::EINVAL)?;
        let old_range = (
            VirtAddr::from(old_addr).floor(),
            VirtAddr::from(old_end).ceil(),
        );

        let update = |inner: &mut MemorySetInner| {
            if may_move {
                inner.mremap_maymove(old_addr, old_len, new_len, new_addr, fixed)
            } else {
                inner.mremap_in_place(old_addr, old_len, new_len)
            }
        };

        if !may_move && new_len >= old_len {
            return self.with_frame_preserving_mut_with_kind(
                #[cfg(feature = "perf")]
                crate::mm::remote_tlb::ShootdownKind::Mremap,
                update,
            );
        }
        if fixed {
            let new_end = new_addr
                .checked_add(new_len)
                .ok_or(crate::utils::SysErrNo::EINVAL)?;
            let new_range = (
                VirtAddr::from(new_addr).floor(),
                VirtAddr::from(new_end).ceil(),
            );
            self.with_ranges_mut(
                #[cfg(feature = "perf")]
                crate::mm::remote_tlb::ShootdownKind::Mremap,
                &[old_range, new_range],
                update,
            )
        } else {
            self.with_range_mut(
                #[cfg(feature = "perf")]
                crate::mm::remote_tlb::ShootdownKind::Mremap,
                old_range.0,
                old_range.1,
                update,
            )
        }
    }

    /// Adjust the process brk VMA without scanning unrelated resident mappings.
    pub fn grow(
        &self,
        grow_size: isize,
        user_heappoint: usize,
        user_heapbottom: usize,
    ) -> Option<usize> {
        if grow_size >= 0 {
            return self.with_frame_preserving_mut(|inner| {
                inner.grow(grow_size, user_heappoint, user_heapbottom)
            });
        }

        let new_addr = user_heappoint.checked_add_signed(grow_size)?;
        let start_vpn = VirtAddr::from(new_addr).ceil();
        let end_vpn = VirtAddr::from(user_heappoint).ceil();
        if start_vpn >= end_vpn {
            self.with_frame_preserving_mut(|inner| {
                inner.grow(grow_size, user_heappoint, user_heapbottom)
            })
        } else {
            self.with_range_mut(
                #[cfg(feature = "perf")]
                crate::mm::remote_tlb::ShootdownKind::Other,
                start_vpn,
                end_vpn,
                |inner| inner.grow(grow_size, user_heappoint, user_heapbottom),
            )
        }
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

    /// Return whether this address space is still published on the current
    /// hart. Direct uaccess uses this lock-free check before touching user
    /// virtual addresses; page-table writers retain frames for every
    /// published hart until their shootdown completes.
    #[inline(always)]
    pub(crate) fn is_current_hart_active(&self) -> bool {
        let hart = crate::arch::cpu::hart_id();
        self.active_harts.load(Ordering::Acquire) & (1usize << hart) != 0
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
        let clear_result = self.with_retained_frames_mut(
            #[cfg(feature = "perf")]
            crate::mm::remote_tlb::ShootdownKind::ForkExec,
            |inner| {
                inner
                    .areas
                    .iter()
                    .flat_map(|area| area.data_frames.values().cloned())
                    .collect()
            },
            |inner| inner.recycle_data_pages(),
        );
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
        self.with_frame_preserving_mut(|inner| {
            inner.insert_framed_area_with_hint(hint, size, map_perm, area_type)
        })
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
        self.with_frame_preserving_mut(|inner| {
            inner.lazy_insert_framed_area_with_hint(hint, size, map_perm, area_type)
        })
    }

    /// Copy a lazily allocated logical area from another address space.
    ///
    /// Snapshot source frame references before acquiring the destination's
    /// update and write locks. Holding the source read guard while entering
    /// `with_mut` would invert `UPDATE_LOCK -> MemorySet::inner` and deadlock
    /// against a concurrent source page fault.
    #[inline(always)]
    pub fn lazy_clone_area(&self, start_vpn: VirtPageNum, another: &MemorySet) {
        let source_pages: Vec<(VirtPageNum, Arc<FrameTracker>)> = {
            let another = another.get_ref();
            let Some(area) = another
                .areas
                .iter()
                .find(|area| area.vpn_range.start() == start_vpn)
            else {
                return;
            };
            area.data_frames
                .iter()
                .filter_map(|(vpn, frame)| {
                    (another.page_table.translate(*vpn) == Some(frame.ppn))
                        .then(|| (*vpn, Arc::clone(frame)))
                })
                .collect()
        };
        self.with_frame_preserving_mut(|inner| inner.lazy_clone_area(start_vpn, &source_pages))
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
