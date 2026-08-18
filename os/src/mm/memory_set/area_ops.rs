//! Logical VM-area management for `MemorySetInner`.
//!
//! This module contains operations that create, remove, locate, or copy
//! `MapArea`s. It deliberately avoids ELF loading, fork construction and mmap
//! details, which live in their own submodules.

use alloc::{sync::Arc, vec::Vec};

use super::MemorySetInner;
use crate::{
    arch::{
        memory_layout::{MMAP_TOP, PAGE_SIZE, USER_HEAP_SIZE},
        page_table::PageTable,
    },
    mm::{
        map_area::MapType, FrameTracker, MapArea, MapAreaType, MapPermission, VPNRange, VirtAddr,
        VirtPageNum,
    },
    syscall::MmapFlags,
};

impl MemorySetInner {
    /// Create an empty user-style address space with a fresh page table.
    pub fn new_bare() -> Self {
        Self {
            page_table: PageTable::new(),
            areas: Vec::new(),
            total_mmap_size: 0,
            mmap_hint: MMAP_TOP,
        }
    }

    /// Create an empty address space whose page table already contains kernel mappings.
    pub fn new_from_kernel() -> Self {
        Self {
            page_table: PageTable::new_from_kernel(),
            areas: Vec::new(),
            total_mmap_size: 0,
            mmap_hint: MMAP_TOP,
        }
    }

    /// Eagerly insert a framed logical area.
    ///
    /// The caller must ensure the new range does not overlap existing areas.
    /// Frame allocation failures are ignored because these call sites are
    /// kernel-internal setup paths where OOM is not expected.
    pub fn insert_framed_area(
        &mut self,
        start_va: VirtAddr,
        end_va: VirtAddr,
        permission: MapPermission,
        area_type: MapAreaType,
    ) {
        self.push(
            MapArea::new(start_va, end_va, MapType::Framed, permission, area_type),
            None,
        )
        .ok();
    }

    /// Insert a framed logical area without mapping pages immediately.
    ///
    /// The pages are allocated later by the page-fault handler.
    pub fn lazy_insert_framed_area(
        &mut self,
        start_va: VirtAddr,
        end_va: VirtAddr,
        permission: MapPermission,
        area_type: MapAreaType,
    ) {
        self.push_lazily(MapArea::new(
            start_va,
            end_va,
            MapType::Framed,
            permission,
            area_type,
        ));
    }

    /// Remove the logical area whose start VPN equals `start_vpn`.
    pub fn remove_area_with_start_vpn(&mut self, start_vpn: VirtPageNum) {
        if let Some((idx, area)) = self
            .areas
            .iter_mut()
            .enumerate()
            .find(|(_, area)| area.vpn_range.start() == start_vpn)
        {
            area.unmap(&mut self.page_table);
            self.areas.remove(idx);
        }
    }

    /// Eagerly insert a framed area below `hint`.
    ///
    /// Returns the selected virtual address range `(start_va, end_va)`.
    pub fn insert_framed_area_with_hint(
        &mut self,
        hint: usize,
        size: usize,
        map_perm: MapPermission,
        area_type: MapAreaType,
    ) -> (usize, usize) {
        let start_va = self.find_insert_addr(hint, size);
        let end_va = start_va + size;
        self.insert_framed_area(
            VirtAddr::from(start_va),
            VirtAddr::from(end_va),
            map_perm,
            area_type,
        );
        (start_va, end_va)
    }

    /// Lazily insert a framed area below `hint`.
    ///
    /// Returns the selected virtual address range `(start_va, end_va)`.
    pub fn lazy_insert_framed_area_with_hint(
        &mut self,
        hint: usize,
        size: usize,
        map_perm: MapPermission,
        area_type: MapAreaType,
    ) -> (usize, usize) {
        let start_va = self.find_insert_addr(hint, size);
        let end_va = start_va + size;
        self.lazy_insert_framed_area(
            VirtAddr::from(start_va),
            VirtAddr::from(end_va),
            map_perm,
            area_type,
        );
        (start_va, end_va)
    }

    /// Find a free range ending at or below `hint`.
    ///
    /// `areas` is kept sorted by start VPN, so a top-down search can move the
    /// candidate below the VMA that it actually intersects and continue from
    /// there. This avoids rescanning the lower VMAs after every collision.
    pub fn find_insert_addr(&self, hint: usize, size: usize) -> usize {
        let pages = match size.checked_add(PAGE_SIZE - 1) {
            Some(size) => size / PAGE_SIZE,
            None => return 0,
        };
        if pages == 0 {
            return 0;
        }
        let mut end_vpn = VirtAddr::from(hint).floor();
        let mut start_vpn = match end_vpn.0.checked_sub(pages) {
            Some(start) => VirtPageNum(start),
            None => return 0,
        };

        for area in self.areas.iter().rev() {
            let (area_start, area_end) = area.vpn_range.range();
            if area_end <= start_vpn {
                break;
            }
            if area_start >= end_vpn {
                continue;
            }

            // Keep the historical one-page separation below an occupied VMA.
            // Besides matching the old recursive search, this protects the
            // guard page expected by MAP_GROWSDOWN stacks.
            let candidate_end = match area_start.0.checked_sub(1) {
                Some(end) => VirtPageNum(end),
                None => return 0,
            };
            end_vpn = candidate_end;
            start_vpn = match end_vpn.0.checked_sub(pages) {
                Some(start) => VirtPageNum(start),
                None => return 0,
            };
        }
        VirtAddr::from(start_vpn).0
    }

    /// Find a non-fixed mmap address using the last successful top-down
    /// position before falling back to the full address-space search.
    pub(crate) fn find_mmap_addr(&mut self, size: usize) -> usize {
        let hint = self.mmap_hint.max(PAGE_SIZE).min(MMAP_TOP);
        let mut addr = self.find_insert_addr(hint, size);
        if addr == 0 && hint != MMAP_TOP {
            addr = self.find_insert_addr(MMAP_TOP, size);
        }
        if addr != 0 {
            self.mmap_hint = addr;
        }
        addr
    }

    /// Use a non-fixed mmap address as a hint when its whole page-rounded
    /// range is currently free. Linux is allowed to choose a different range
    /// when the hint collides, but callers such as mremap users rely on a
    /// freshly reserved hole being honored.
    pub(crate) fn try_mmap_hint(&mut self, hint: usize, size: usize) -> Option<usize> {
        if hint == 0 {
            return None;
        }
        let start = hint & !(PAGE_SIZE - 1);
        let end = start.checked_add(size)?;
        if start == 0 || end > MMAP_TOP {
            return None;
        }
        let start_vpn = VirtAddr::from(start).floor();
        let end_vpn = VirtAddr::from(end).ceil();
        if self.areas.iter().any(|area| {
            let (area_start, area_end) = area.vpn_range.range();
            area_start < end_vpn && start_vpn < area_end
        }) {
            return None;
        }
        self.mmap_hint = start;
        Some(start)
    }

    /// Find a free mmap range whose start is aligned to `align` bytes.
    pub(crate) fn find_mmap_addr_aligned(&mut self, size: usize, align: usize) -> usize {
        if align == 0 || !align.is_power_of_two() {
            return 0;
        }
        let search_size = match size.checked_add(align - 1) {
            Some(value) => value,
            None => return 0,
        };
        let hint = self.mmap_hint.max(PAGE_SIZE).min(MMAP_TOP);
        for search_hint in [hint, MMAP_TOP] {
            if search_hint != MMAP_TOP && search_hint == hint && hint == MMAP_TOP {
                continue;
            }
            let candidate = self.find_insert_addr(search_hint, search_size);
            if candidate == 0 {
                continue;
            }
            let Some(addr_base) = candidate.checked_add(align - 1) else {
                continue;
            };
            let addr = addr_base & !(align - 1);
            let Some(end) = addr.checked_add(size) else {
                continue;
            };
            if end > search_hint || end > MMAP_TOP {
                continue;
            }
            self.mmap_hint = addr;
            return addr;
        }
        0
    }

    /// Find an area whose range exactly equals `[l, r)`.
    pub fn find_area_by_range(&mut self, l: VirtPageNum, r: VirtPageNum) -> Option<&mut MapArea> {
        let target = (l, r);
        self.areas
            .iter_mut()
            .find(|area| area.vpn_range.range() == target)
    }

    /// Adjust the process brk area and return the new heap pointer.
    pub fn grow(
        &mut self,
        grow_size: isize,
        user_heappoint: usize,
        user_heapbottom: usize,
    ) -> Option<usize> {
        let new_addr = user_heappoint.checked_add_signed(grow_size)?;
        let new_vpn: VirtPageNum = VirtAddr::from(new_addr).ceil();
        let old_vpn: VirtPageNum = VirtAddr::from(user_heappoint).ceil();
        if grow_size > 0 {
            let user_vpn_top: VirtPageNum = ((user_heapbottom + USER_HEAP_SIZE) / PAGE_SIZE).into();
            if new_vpn >= user_vpn_top {
                return None;
            }
            if old_vpn >= new_vpn {
                return Some(new_addr);
            }

            // A MAP_FIXED mapping may occupy part of the logical brk span.
            // Keep the user-visible brk pointer independent from those holes:
            // only materialize Brk VMAs in the newly grown, currently free
            // subranges. This yields the Linux layout "brk | mmap | brk".
            let mut free_ranges = Vec::new();
            let mut cursor = old_vpn;
            for area in &self.areas {
                if area.area_type == MapAreaType::Brk {
                    continue;
                }
                let (start, end) = area.vpn_range.range();
                if end <= cursor || start >= new_vpn {
                    continue;
                }
                if cursor < start {
                    free_ranges.push((cursor, start.min(new_vpn)));
                }
                if end > cursor {
                    cursor = end.min(new_vpn);
                }
                if cursor >= new_vpn {
                    break;
                }
            }
            if cursor < new_vpn {
                free_ranges.push((cursor, new_vpn));
            }
            for (start, end) in free_ranges {
                if start < end {
                    self.push_lazily(MapArea::new(
                        VirtAddr::from(start),
                        VirtAddr::from(end),
                        MapType::Framed,
                        MapPermission::R | MapPermission::W | MapPermission::U,
                        MapAreaType::Brk,
                    ));
                }
            }
        } else if grow_size < 0 {
            if new_addr < user_heapbottom {
                return None;
            }
            self.remove_brk_range(new_vpn, old_vpn);
        }
        Some(new_addr)
    }

    /// Remove only the brk portions covered by a range, leaving unrelated
    /// fixed mappings intact. The logical brk pointer is owned by TaskInner,
    /// so callers may use this to create or shrink holes in the heap layout.
    pub(crate) fn remove_brk_range(&mut self, start_vpn: VirtPageNum, end_vpn: VirtPageNum) {
        if start_vpn >= end_vpn {
            return;
        }
        while let Some(idx) = self.areas.iter().position(|area| {
            if area.area_type != MapAreaType::Brk {
                return false;
            }
            let (start, end) = area.vpn_range.range();
            start < end_vpn && end > start_vpn
        }) {
            let (area_start, area_end) = self.areas[idx].vpn_range.range();
            let removed_start = area_start.max(start_vpn);
            let removed_end = area_end.min(end_vpn);
            for vpn in VPNRange::new(removed_start, removed_end) {
                self.areas[idx].unmap_one(&mut self.page_table, vpn);
            }

            if area_start >= start_vpn && area_end <= end_vpn {
                self.areas.remove(idx);
            } else if area_start < start_vpn && area_end <= end_vpn {
                self.areas[idx].vpn_range = VPNRange::new(area_start, start_vpn);
            } else if area_start >= start_vpn && area_end > end_vpn {
                self.areas[idx].vpn_range = VPNRange::new(end_vpn, area_end);
            } else {
                let mut right_area = MapArea::from_another(&self.areas[idx]);
                right_area.vpn_range = VPNRange::new(end_vpn, area_end);
                let right_keys: Vec<VirtPageNum> = self.areas[idx]
                    .data_frames
                    .range(end_vpn..)
                    .map(|(vpn, _)| *vpn)
                    .collect();
                for vpn in right_keys {
                    if let Some(frame) = self.areas[idx].data_frames.remove(&vpn) {
                        right_area.data_frames.insert(vpn, frame);
                    }
                }
                self.areas[idx].vpn_range = VPNRange::new(area_start, start_vpn);
                self.insert_area_sorted_unmerged(right_area);
            }
        }
    }

    /// Copy snapshotted pages into a lazily allocated area, faulting destination
    /// pages as needed.
    pub fn lazy_clone_area(
        &mut self,
        start_vpn: VirtPageNum,
        source_pages: &[(VirtPageNum, Arc<FrameTracker>)],
    ) {
        let (areas, this_page_table) = (&mut self.areas, &mut self.page_table);
        let this_area = if let Some(area) = areas
            .iter_mut()
            .find(|area| area.vpn_range.start() == start_vpn)
        {
            area
        } else {
            return;
        };
        for (vpn, source_frame) in source_pages {
            if !this_area.vpn_range.contains_vpn(*vpn) {
                continue;
            }

            let dst_ppn = match this_page_table.translate(*vpn) {
                Some(ppn) => ppn,
                None => match this_area.map_one(this_page_table, *vpn) {
                    Some(ppn) => ppn,
                    None => continue,
                },
            };

            dst_ppn
                .bytes_array_mut()
                .copy_from_slice(source_frame.ppn.bytes_array());
        }
    }

    /// Insert an area by start VPN without coalescing it with its neighbors.
    ///
    /// VMA transformation paths use this while an existing area is being
    /// split. Coalescing at that point would undo the split and invalidate the
    /// caller's area index before the operation has finished.
    pub(super) fn insert_area_sorted_unmerged(&mut self, map_area: MapArea) -> usize {
        let start_vpn = map_area.vpn_range.start();
        let index = self
            .areas
            .binary_search_by_key(&start_vpn, |area| area.vpn_range.start())
            .unwrap_or_else(|index| index);
        self.areas.insert(index, map_area);
        index
    }

    /// Insert an area by start VPN and merge compatible adjacent anonymous
    /// private VMAs. Keeping this invariant in one helper prevents ordinary
    /// allocation paths from reintroducing an unsorted `areas` vector.
    fn insert_area_sorted(&mut self, map_area: MapArea) {
        let index = self.insert_area_sorted_unmerged(map_area);
        self.merge_adjacent_areas(index);
    }

    fn can_merge_areas(left: &MapArea, right: &MapArea) -> bool {
        left.vpn_range.end() == right.vpn_range.start()
            && left.map_type == right.map_type
            && left.map_perm == right.map_perm
            && left.area_type == right.area_type
            && left.mmap_flags == right.mmap_flags
            && left.groupid == 0
            && right.groupid == 0
            && left.mmap_file.is_anonymous()
            && right.mmap_file.is_anonymous()
            && left.mmap_flags.contains(MmapFlags::MAP_PRIVATE)
            && left.mmap_flags.contains(MmapFlags::MAP_ANONYMOUS)
    }

    fn merge_adjacent_areas(&mut self, mut index: usize) {
        loop {
            if index > 0 && Self::can_merge_areas(&self.areas[index - 1], &self.areas[index]) {
                let mut right = self.areas.remove(index);
                let left = &mut self.areas[index - 1];
                left.vpn_range = VPNRange::new(left.vpn_range.start(), right.vpn_range.end());
                left.data_frames.append(&mut right.data_frames);
                index -= 1;
                continue;
            }
            if index + 1 < self.areas.len()
                && Self::can_merge_areas(&self.areas[index], &self.areas[index + 1])
            {
                let mut right = self.areas.remove(index + 1);
                let left = &mut self.areas[index];
                left.vpn_range = VPNRange::new(left.vpn_range.start(), right.vpn_range.end());
                left.data_frames.append(&mut right.data_frames);
                continue;
            }
            break;
        }
    }

    /// Push a `MapArea` with eager frame allocation.
    pub(crate) fn push(&mut self, mut map_area: MapArea, data: Option<&[u8]>) -> Result<(), ()> {
        map_area.map(&mut self.page_table)?;
        if let Some(data) = data {
            map_area.copy_data(&mut self.page_table, data, 0);
        }
        self.insert_area_sorted(map_area);
        Ok(())
    }

    /// Push an eagerly allocated `MapArea` and copy data starting at `offset`.
    pub(crate) fn push_with_offset(
        &mut self,
        mut map_area: MapArea,
        offset: usize,
        data: Option<&[u8]>,
    ) -> Result<(), ()> {
        map_area.map(&mut self.page_table)?;
        if let Some(data) = data {
            map_area.copy_data(&mut self.page_table, data, offset);
        }
        self.insert_area_sorted(map_area);
        Ok(())
    }

    /// Push an area backed by already allocated frames.
    pub(crate) fn push_with_given_frames(
        &mut self,
        mut map_area: MapArea,
        frames: Vec<Arc<FrameTracker>>,
    ) {
        map_area.map_given_frames(&mut self.page_table, frames);
        self.insert_area_sorted(map_area);
    }

    /// Add a `MapArea` without immediately mapping pages.
    pub fn push_lazily(&mut self, map_area: MapArea) {
        self.insert_area_sorted(map_area);
    }
}
