//! Logical VM-area management for `MemorySetInner`.
//!
//! This module contains operations that create, remove, locate, or copy
//! `MapArea`s. It deliberately avoids ELF loading, fork construction and mmap
//! details, which live in their own submodules.

use alloc::{sync::Arc, vec::Vec};

use super::MemorySetInner;
use crate::{
    arch::{
        memory_layout::{PAGE_SIZE, USER_HEAP_SIZE},
        page_table::PageTable,
        tlb::tlb_invalidate,
    },
    mm::{
        map_area::MapType, FrameTracker, MapArea, MapAreaType, MapPermission, VPNRange, VirtAddr,
        VirtPageNum,
    },
};

impl MemorySetInner {
    /// Create an empty user-style address space with a fresh page table.
    pub fn new_bare() -> Self {
        Self {
            page_table: PageTable::new(),
            areas: Vec::new(),
            total_mmap_size: 0,
        }
    }

    /// Create an empty address space whose page table already contains kernel mappings.
    pub fn new_from_kernel() -> Self {
        Self {
            page_table: PageTable::new_from_kernel(),
            areas: Vec::new(),
            total_mmap_size: 0,
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
        tlb_invalidate();
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
    /// The returned address is page-aligned. Existing areas are treated as
    /// closed obstacles; if the candidate overlaps one, the search continues
    /// below that area.
    pub fn find_insert_addr(&self, hint: usize, size: usize) -> usize {
        let end_vpn = VirtAddr::from(hint).floor();
        let start_vpn = VirtAddr::from(hint - size).floor();

        for area in self.areas.iter() {
            let (start, end) = area.vpn_range.range();
            if end_vpn > start && start_vpn < end {
                let new_hint = VirtAddr::from(start_vpn).0 - PAGE_SIZE;
                return self.find_insert_addr(new_hint, size);
            }
        }
        VirtAddr::from(start_vpn).0
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
        let heap_bottom_vpn: VirtPageNum = (user_heapbottom / PAGE_SIZE).into();
        let (areas, page_table) = (&mut self.areas, &mut self.page_table);
        let area_idx = areas
            .iter()
            .position(|area| area.area_type == MapAreaType::Brk)
            .unwrap();
        let old_end_vpn = areas[area_idx].vpn_range.end();
        if grow_size > 0 {
            let user_vpn_top: VirtPageNum = ((user_heapbottom + USER_HEAP_SIZE) / PAGE_SIZE).into();
            if new_vpn >= user_vpn_top {
                return None;
            }
            // MAP_FIXED may have installed a VMA inside the reserved brk range.
            // Do not let a later brk expansion create overlapping VMAs.
            if areas.iter().any(|other| {
                if other.area_type == MapAreaType::Brk {
                    return false;
                }
                let (start, end) = other.vpn_range.range();
                start < new_vpn && heap_bottom_vpn < end
            }) {
                return None;
            }
            areas[area_idx].vpn_range = VPNRange::new(heap_bottom_vpn, new_vpn);
        } else {
            if new_addr < user_heapbottom {
                return None;
            }
            areas[area_idx].vpn_range = VPNRange::new(heap_bottom_vpn, new_vpn);
            // Clear the complete old tail, not only data_frames entries.  A
            // stale PTE without a FrameTracker must not survive shrink/grow
            // and later become an unpinned COW source during fork.
            for vpn in VPNRange::new(new_vpn, old_end_vpn) {
                page_table.unmap(vpn);
                areas[area_idx].data_frames.remove(&vpn);
            }
        }
        tlb_invalidate();
        Some(new_addr)
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

    /// Push a `MapArea` with eager frame allocation.
    pub(crate) fn push(&mut self, mut map_area: MapArea, data: Option<&[u8]>) -> Result<(), ()> {
        map_area.map(&mut self.page_table)?;
        if let Some(data) = data {
            map_area.copy_data(&mut self.page_table, data, 0);
        }
        self.areas.push(map_area);
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
        self.areas.push(map_area);
        Ok(())
    }

    /// Push an area backed by already allocated frames.
    pub(crate) fn push_with_given_frames(
        &mut self,
        mut map_area: MapArea,
        frames: Vec<Arc<FrameTracker>>,
    ) {
        map_area.map_given_frames(&mut self.page_table, frames);
        self.areas.push(map_area);
    }

    /// Add a `MapArea` without immediately mapping pages.
    pub fn push_lazily(&mut self, map_area: MapArea) {
        self.areas.push(map_area);
    }
}
