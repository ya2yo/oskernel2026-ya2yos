//! mmap / munmap / mprotect / shm / page-fault handlers.
//!
//! These are the methods of [`MemorySetInner`] that implement the user-facing
//! virtual memory operations: allocating anonymous/file-backed mappings,
//! unmapping, protecting, shared memory attach, and the associated
//! lazy- / COW- / mmap- page-fault handlers.

use crate::mm::group::GROUP_SHARE;
use crate::mm::map_area::MapType;
use crate::mm::memory_set::MemorySetInner;
use crate::mm::page_fault_handler::{
    cow_page_fault, lazy_page_fault, mmap_read_page_fault, mmap_write_page_fault,
};
use super::{
    translated_byte_buffer, FrameTracker, MapArea, MapAreaType, MapPermission, PhysAddr, UserBuffer,
    VPNRange, VirtAddr, VirtPageNum,
};
use crate::arch::memory_layout::{MMAP_TOP, PAGE_SIZE, PAGE_SIZE_BITS};
use crate::arch::page_table::PageTable;
use crate::arch::tlb::tlb_invalidate;
use crate::fs::{File, OSFile, OpenFlags, SEEK_CUR, SEEK_SET};
use crate::syscall::MmapFlags;
use crate::trap::trap_types::*;
use crate::utils::SyscallRet;
use alloc::{string::String, sync::Arc, vec::Vec};
use log::debug;

impl MemorySetInner {
    pub fn shm(
        &mut self,
        addr: usize,
        size: usize,
        map_perm: MapPermission,
        pages: Vec<Arc<FrameTracker>>,
    ) -> usize {
        if addr == 0 {
            let va = self.find_insert_addr(MMAP_TOP, size);
            self.push_with_given_frames(
                MapArea::new(va.into(), (va + size).into(), MapType::Framed, map_perm, MapAreaType::Shm),
                pages,
            );
            return va;
        }
        panic!("[shm_attach] unimplement attach addr");
    }

    /// mmap
    pub fn mmap(
        &mut self,
        addr: usize,
        len: usize,
        map_perm: MapPermission,
        flags: MmapFlags,
        file: Option<Arc<OSFile>>,
        off: usize,
    ) -> usize {
        debug!("[mmap] addr={:x}, len={}, map_perm={:?}, flags={:?}", addr, len, map_perm, flags);
        if flags.contains(MmapFlags::MAP_FIXED) {
            let start_vpn = VirtAddr::from(addr).floor();
            let end_vpn = VirtAddr::from(addr + len).ceil();
            let need_split = self.areas.iter().any(|area| {
                let (l, r) = area.vpn_range.range();
                if l <= start_vpn && end_vpn <= r {
                    !(l == start_vpn && r == end_vpn && map_perm == area.map_perm)
                } else {
                    false
                }
            });
            if need_split {
                self.mprotect(start_vpn, end_vpn, map_perm, file, off, true);
            } else {
                self.push_lazily(MapArea::new_mmap(
                    VirtAddr::from(addr), VirtAddr::from(addr + len),
                    MapType::Framed, map_perm, MapAreaType::Mmap,
                    file, off, flags,
                ));
            }
            return addr;
        }
        let addr = self.find_insert_addr(MMAP_TOP, len);
        let area_type = if flags.contains(MmapFlags::MAP_STACK) {
            MapAreaType::Stack
        } else {
            MapAreaType::Mmap
        };
        self.push_lazily(MapArea::new_mmap(
            VirtAddr::from(addr), VirtAddr::from(addr + len),
            MapType::Framed, map_perm, area_type,
            file, off, flags,
        ));
        addr
    }

    /// munmap
    pub fn munmap(&mut self, addr: usize, len: usize) -> SyscallRet {
        debug!("[munmap] addr={:x}, len={}", addr, len);
        let start_vpn = VirtPageNum::from(VirtAddr::from(addr));
        let end_vpn = VirtPageNum::from(VirtAddr::from(addr + len));
        while let Some((idx, area)) = self
            .areas
            .iter_mut()
            .enumerate()
            .filter(|(_, area)| area.area_type == MapAreaType::Mmap)
            .find(|(_, area)| {
                let (start, end) = area.vpn_range.range();
                start >= start_vpn && end <= end_vpn
            })
        {
            if area.mmap_flags.contains(MmapFlags::MAP_SHARED)
                && area.map_perm.contains(MapPermission::W)
                && area.mmap_file.file.is_some()
            {
                let mut wb_range: Vec<(VirtPageNum, VirtPageNum)> = Vec::new();
                VPNRange::new(start_vpn, end_vpn)
                    .into_iter()
                    .for_each(|vpn| {
                        if area.data_frames.contains_key(&vpn) {
                            if wb_range.is_empty() {
                                wb_range.push((vpn, VirtPageNum(vpn.0 + 1)));
                            } else {
                                let end_range = wb_range.pop().unwrap();
                                if end_range.1 == vpn {
                                    wb_range.push((end_range.0, VirtPageNum(vpn.0 + 1)));
                                } else {
                                    wb_range.push(end_range);
                                    wb_range.push((vpn, VirtPageNum(vpn.0 + 1)));
                                }
                            }
                        }
                    });
                let file = area.mmap_file.file.clone().unwrap();
                let off = file.lseek(0, SEEK_CUR).unwrap();
                wb_range.into_iter().for_each(|(start_vpn, end_vpn)| {
                    let start_addr: usize = VirtAddr::from(start_vpn).into();
                    let mapped_len: usize = (end_vpn.0 - start_vpn.0) * PAGE_SIZE;
                    let buf = UserBuffer {
                        buffers: translated_byte_buffer(
                            self.page_table.token(),
                            start_addr as *const u8,
                            mapped_len,
                        )
                        .unwrap(),
                    };
                    file.lseek((start_addr - addr) as isize, SEEK_SET);
                    file.write(buf);
                });
                file.lseek(off as isize, SEEK_SET);
            }
            for vpn in VPNRange::new(start_vpn, end_vpn) {
                area.unmap_one(&mut self.page_table, vpn);
            }
            let area_end_vpn = area.vpn_range.end();
            if area_end_vpn <= end_vpn {
                self.areas.remove(idx);
            } else {
                area.vpn_range = VPNRange::new(end_vpn, area_end_vpn);
            }
            tlb_invalidate();
        }
        Ok(0)
    }

    /// 修改一段虚拟地址空间的访问权限
    pub fn mprotect(
        &mut self,
        start_vpn: VirtPageNum,
        end_vpn: VirtPageNum,
        map_perm: MapPermission,
        file: Option<Arc<OSFile>>,
        offset: usize,
        if_mmap: bool,
    ) {
        let mut new_areas = Vec::new();
        for area in self.areas.iter_mut() {
            let (start, end) = area.vpn_range.range();
            if start >= start_vpn && end <= end_vpn {
                area.map_perm = map_perm;
                if if_mmap { area.mmap_file.file = file.clone(); }
                if offset != usize::MAX { area.mmap_file.offset = offset as usize; }
                continue;
            } else if start < start_vpn && end > start_vpn && end <= end_vpn {
                let mut new_area = MapArea::from_another(area);
                new_area.map_perm = map_perm;
                new_area.vpn_range = VPNRange::new(start_vpn, end);
                if if_mmap { new_area.mmap_file.file = file.clone(); }
                if offset != usize::MAX { new_area.mmap_file.offset = offset as usize; }
                area.vpn_range = VPNRange::new(start, start_vpn);
                GROUP_SHARE.lock().add_area(new_area.groupid);
                while !area.data_frames.is_empty() {
                    let page = area.data_frames.pop_last().unwrap();
                    new_area.data_frames.insert(page.0, page.1);
                    if page.0 == start_vpn { break; }
                }
                new_areas.push(new_area);
                continue;
            } else if start >= start_vpn && start < end_vpn && end > end_vpn {
                let mut new_area = MapArea::from_another(area);
                new_area.map_perm = map_perm;
                new_area.vpn_range = VPNRange::new(start, end_vpn);
                if if_mmap { new_area.mmap_file.file = file.clone(); }
                if offset != usize::MAX { new_area.mmap_file.offset = offset as usize; }
                area.vpn_range = VPNRange::new(end_vpn, end);
                GROUP_SHARE.lock().add_area(new_area.groupid);
                while !area.data_frames.is_empty() {
                    let page = area.data_frames.pop_first().unwrap();
                    if page.0 >= end_vpn { area.data_frames.insert(page.0, page.1); break; }
                    new_area.data_frames.insert(page.0, page.1);
                }
                new_areas.push(new_area);
                continue;
            } else if start < start_vpn && end > end_vpn {
                let mut front_area = MapArea::from_another(area);
                let mut back_area = MapArea::from_another(area);
                area.map_perm = map_perm;
                front_area.vpn_range = VPNRange::new(start, start_vpn);
                back_area.vpn_range = VPNRange::new(end_vpn, end);
                area.vpn_range = VPNRange::new(start_vpn, end_vpn);
                if if_mmap { area.mmap_file.file = file.clone(); }
                if offset != usize::MAX { area.mmap_file.offset = offset as usize; }
                GROUP_SHARE.lock().add_area(front_area.groupid);
                GROUP_SHARE.lock().add_area(back_area.groupid);
                while !area.data_frames.is_empty() {
                    let page = area.data_frames.pop_first().unwrap();
                    if page.0 >= start_vpn { area.data_frames.insert(page.0, page.1); break; }
                    front_area.data_frames.insert(page.0, page.1);
                }
                while !area.data_frames.is_empty() {
                    let page = area.data_frames.pop_last().unwrap();
                    if page.0 < end_vpn { area.data_frames.insert(page.0, page.1); break; }
                    back_area.data_frames.insert(page.0, page.1);
                }
                new_areas.push(front_area);
                new_areas.push(back_area);
            }
        }
        for area in new_areas { self.areas.push(area); }
        for vpn in start_vpn.0..=end_vpn.0 {
            self.page_table.handle_mprotect(vpn.into(), map_perm);
        }
        tlb_invalidate();
    }

    pub fn lazy_page_fault(&mut self, vpn: VirtPageNum, scause: Trap) -> bool {
        // log::info!(
        // "fault vpn={:?} va={:#x}",
        //     vpn,
        //     vpn.0 << PAGE_SIZE_BITS
        // );
        let ppn = self.page_table.translate(vpn);
        if !ppn.is_none() { return false; }
        // mmap
        if let Some(area) = self.areas.iter_mut()
            .filter(|area| area.area_type == MapAreaType::Mmap)
            .find(|area| { let (start, end) = area.vpn_range.range(); start <= vpn && vpn < end })
        {
            if scause == Trap::Exception(Exception::LoadPageFault)
                || scause == Trap::Exception(Exception::FetchInstructionPageFault)
            {
                mmap_read_page_fault(vpn.into(), &mut self.page_table, area);
            } else {
                mmap_write_page_fault(vpn.into(), &mut self.page_table, area);
            }
            return true;
        }
        // brk or stack
        if let Some(area) = self.areas.iter_mut()
            .filter(|area| area.area_type == MapAreaType::Brk || area.area_type == MapAreaType::Stack)
            .find(|area| { let (start, end) = area.vpn_range.range(); start <= vpn && vpn < end })
        {
            lazy_page_fault(vpn.into(), &mut self.page_table, area);
            return true;
        }
        false
    }

    pub fn cow_page_fault(&mut self, vpn: VirtPageNum, scause: Trap) -> bool {
        if scause == Trap::Exception(Exception::LoadPageFault)
            || scause == Trap::Exception(Exception::FetchInstructionPageFault)
        { return false; }
        if let Some(area) = self.areas.iter_mut()
            .filter(|area| {
                area.area_type == MapAreaType::Elf
                    || area.area_type == MapAreaType::Brk
                    || area.area_type == MapAreaType::Mmap
            })
            .find(|area| { let (start, end) = area.vpn_range.range(); start <= vpn && vpn < end })
        {
            if cow_page_fault(vpn.into(), &mut self.page_table, area) {
                return true;
            }
        }
        false
    }
}
