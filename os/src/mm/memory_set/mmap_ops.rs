//! mmap / munmap / mprotect / shm / page-fault handlers.
//!
//! These are the methods of [`MemorySetInner`] that implement the user-facing
//! virtual memory operations: allocating anonymous/file-backed mappings,
//! unmapping, protecting, shared memory attach, and the associated
//! lazy- / COW- / mmap- page-fault handlers.

use super::{
    read_user_bytes_direct_into, user_buffer_from_kernel, FrameTracker, MapArea, MapAreaType,
    MapPermission, PhysAddr, UserBuffer, VPNRange, VirtAddr, VirtPageNum,
};
use crate::arch::memory_layout::{MAX_MMAP_SIZE, MMAP_TOP, PAGE_SIZE, PAGE_SIZE_BITS};
use crate::arch::page_table::PageTable;
use crate::arch::tlb::tlb_invalidate;
use crate::fs::{File, OSFile, OpenFlags, SEEK_CUR, SEEK_SET};
use crate::mm::group::GROUP_SHARE;
use crate::mm::map_area::MapType;
use crate::mm::memory_set::MemorySetInner;
use crate::mm::page_fault_handler::{
    cow_page_fault, lazy_page_fault, mmap_read_page_fault, mmap_write_page_fault,
};
use crate::syscall::MmapFlags;
use crate::trap::trap_types::*;
use crate::utils::{SysErrNo, SyscallRet};
use alloc::vec;
use alloc::{string::String, sync::Arc, vec::Vec};
use log::{debug, warn};

const MMAP_WRITEBACK_CHUNK_SIZE: usize = 0x10000; // 64KB

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
                MapArea::new(
                    va.into(),
                    (va + size).into(),
                    MapType::Framed,
                    map_perm,
                    MapAreaType::Shm,
                ),
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
        debug!(
            "[mmap] addr={:x}, len={}, map_perm={:?}, flags={:?}",
            addr, len, map_perm, flags
        );
        if flags.contains(MmapFlags::MAP_FIXED) || flags.contains(MmapFlags::MAP_FIXED_NOREPLACE) {
            // 检查 addr + len 是否溢出
            let end_addr = match addr.checked_add(len) {
                Some(v) => v,
                None => return 0,
            };
            // 如果 end_addr 比 addr 还小，说明发生了回绕，拒绝
            if end_addr < addr {
                return 0;
            }
            let start_vpn = VirtAddr::from(addr).floor();
            let end_vpn = VirtAddr::from(end_addr).ceil();
            // MAP_FIXED_NOREPLACE: 如果任何现有映射与目标范围重叠，则失败
            if flags.contains(MmapFlags::MAP_FIXED_NOREPLACE) {
                let has_overlap = self.areas.iter().any(|area| {
                    let (l, r) = area.vpn_range.range();
                    l < end_vpn && start_vpn < r
                });
                if has_overlap {
                    debug!(
                        "[mmap] MAP_FIXED_NOREPLACE overlap: [{:x}, {:x})",
                        addr, end_addr
                    );
                    return 0;
                }
            }
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
                    VirtAddr::from(addr),
                    VirtAddr::from(end_addr),
                    MapType::Framed,
                    map_perm,
                    MapAreaType::Mmap,
                    file,
                    off,
                    flags,
                ));
            }
            // MAP_FIXED / MAP_FIXED_NOREPLACE 使用指定地址，不计入 mmap 总量
            return addr;
        }
        // Reject if this allocation would exceed the per-process mmap limit.
        // Without this check, runaway mmap (e.g. glibc ungetc on a char device)
        // can allocate unlimited virtual space and exhaust physical memory
        // through subsequent lazy page faults.
        if self.total_mmap_size + len > MAX_MMAP_SIZE {
            debug!(
                "[mmap] ENOMEM: total_mmap_size={}, request={}, max={}",
                self.total_mmap_size, len, MAX_MMAP_SIZE
            );
            return 0; // signals failure to sys_mmap, which returns ENOMEM
        }
        let addr = self.find_insert_addr(MMAP_TOP, len);
        if addr == 0 {
            return 0; // no space found
        }
        let area_type = if flags.contains(MmapFlags::MAP_STACK) {
            MapAreaType::Stack
        } else {
            MapAreaType::Mmap
        };
        self.push_lazily(MapArea::new_mmap(
            VirtAddr::from(addr),
            VirtAddr::from(addr + len),
            MapType::Framed,
            map_perm,
            area_type,
            file,
            off,
            flags,
        ));
        self.total_mmap_size += len;
        addr
    }

    /// munmap
    pub fn munmap(&mut self, addr: usize, len: usize) -> SyscallRet {
        debug!("[munmap] addr={:x}, len={}", addr, len);
        // 检查 addr + len 是否溢出
        let end_addr = match addr.checked_add(len) {
            Some(v) => v,
            None => return Err(SysErrNo::EINVAL),
        };
        let start_vpn = VirtAddr::from(addr).floor();
        let end_vpn = VirtAddr::from(end_addr).ceil();
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
                let file = area.mmap_file.file.clone().unwrap();
                if file.inode.link_cnt()? > 0 {
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
                    let off = file.lseek(0, SEEK_CUR).unwrap();
                    let map_base: usize = VirtAddr::from(area.vpn_range.start()).into();
                    let file_base = area.mmap_file.offset;
                    for (start_vpn, end_vpn) in wb_range {
                        let start_addr: usize = VirtAddr::from(start_vpn).into();
                        let mapped_len: usize = (end_vpn.0 - start_vpn.0) * PAGE_SIZE;
                        let mut written = 0;
                        while written < mapped_len {
                            let chunk_len = MMAP_WRITEBACK_CHUNK_SIZE.min(mapped_len - written);
                            let mut kernel_buf = vec![0u8; chunk_len];
                            if read_user_bytes_direct_into(
                                self.page_table.token(),
                                start_addr + written,
                                &mut kernel_buf,
                            )
                            .is_none()
                            {
                                break;
                            }
                            let buf = unsafe { user_buffer_from_kernel(&mut kernel_buf) };
                            file.lseek(
                                (file_base + (start_addr - map_base) + written) as isize,
                                SEEK_SET,
                            )
                            .unwrap();
                            let ret = file.write(buf)?;
                            if ret == 0 {
                                break;
                            }
                            written += ret;
                        }
                    }
                    file.lseek(off as isize, SEEK_SET).unwrap();
                } else {
                    debug!(
                        "[munmap] skip writeback for unlinked shared mapping: {}",
                        file.inode.path()
                    );
                }
            }
            for vpn in VPNRange::new(start_vpn, end_vpn) {
                area.unmap_one(&mut self.page_table, vpn);
            }
            let area_end_vpn = area.vpn_range.end();
            if area_end_vpn <= end_vpn {
                let area_size = (area_end_vpn.0 - area.vpn_range.start().0) * PAGE_SIZE;
                self.total_mmap_size = self.total_mmap_size.saturating_sub(area_size);
                self.areas.remove(idx);
            } else {
                let trimmed = (end_vpn.0 - area.vpn_range.start().0) * PAGE_SIZE;
                self.total_mmap_size = self.total_mmap_size.saturating_sub(trimmed);
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
        // 防御性检查：如果范围无效则直接返回
        if start_vpn >= end_vpn {
            warn!(
                "[mprotect] invalid range: start_vpn={:?} >= end_vpn={:?}",
                start_vpn, end_vpn
            );
            return;
        }
        let mut new_areas = Vec::new();
        for area in self.areas.iter_mut() {
            let (start, end) = area.vpn_range.range();
            if start >= start_vpn && end <= end_vpn {
                area.map_perm = map_perm;
                if if_mmap {
                    area.mmap_file.file = file.clone();
                }
                if offset != usize::MAX {
                    area.mmap_file.offset = offset as usize;
                }
                continue;
            } else if start < start_vpn && end > start_vpn && end <= end_vpn {
                let mut new_area = MapArea::from_another(area);
                new_area.map_perm = map_perm;
                new_area.vpn_range = VPNRange::new(start_vpn, end);
                if if_mmap {
                    new_area.mmap_file.file = file.clone();
                }
                if offset != usize::MAX {
                    new_area.mmap_file.offset = offset as usize;
                }
                area.vpn_range = VPNRange::new(start, start_vpn);
                GROUP_SHARE.lock().add_area(new_area.groupid);
                while !area.data_frames.is_empty() {
                    let page = area.data_frames.pop_last().unwrap();
                    new_area.data_frames.insert(page.0, page.1);
                    if page.0 == start_vpn {
                        break;
                    }
                }
                new_areas.push(new_area);
                continue;
            } else if start >= start_vpn && start < end_vpn && end > end_vpn {
                let mut new_area = MapArea::from_another(area);
                new_area.map_perm = map_perm;
                new_area.vpn_range = VPNRange::new(start, end_vpn);
                if if_mmap {
                    new_area.mmap_file.file = file.clone();
                }
                if offset != usize::MAX {
                    new_area.mmap_file.offset = offset as usize;
                }
                area.vpn_range = VPNRange::new(end_vpn, end);
                GROUP_SHARE.lock().add_area(new_area.groupid);
                while !area.data_frames.is_empty() {
                    let page = area.data_frames.pop_first().unwrap();
                    if page.0 >= end_vpn {
                        area.data_frames.insert(page.0, page.1);
                        break;
                    }
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
                if if_mmap {
                    area.mmap_file.file = file.clone();
                }
                if offset != usize::MAX {
                    area.mmap_file.offset = offset as usize;
                }
                GROUP_SHARE.lock().add_area(front_area.groupid);
                GROUP_SHARE.lock().add_area(back_area.groupid);
                while !area.data_frames.is_empty() {
                    let page = area.data_frames.pop_first().unwrap();
                    if page.0 >= start_vpn {
                        area.data_frames.insert(page.0, page.1);
                        break;
                    }
                    front_area.data_frames.insert(page.0, page.1);
                }
                while !area.data_frames.is_empty() {
                    let page = area.data_frames.pop_last().unwrap();
                    if page.0 < end_vpn {
                        area.data_frames.insert(page.0, page.1);
                        break;
                    }
                    back_area.data_frames.insert(page.0, page.1);
                }
                new_areas.push(front_area);
                new_areas.push(back_area);
            }
        }
        for area in new_areas {
            self.areas.push(area);
        }
        for vpn in start_vpn.0..=end_vpn.0 {
            self.page_table.handle_mprotect(vpn.into(), map_perm);
        }
        tlb_invalidate();
    }

    pub fn lazy_page_fault(&mut self, vpn: VirtPageNum, scause: Trap) -> bool {
        // debug!("[lazy_page_fault] vpn={:?} scause={:?}", vpn, scause);
        let ppn = self.page_table.translate(vpn);
        if !ppn.is_none() {
            return false;
        }
        // mmap
        if let Some(area) = self
            .areas
            .iter_mut()
            .filter(|area| area.area_type == MapAreaType::Mmap)
            .find(|area| {
                let (start, end) = area.vpn_range.range();
                start <= vpn && vpn < end
            })
        {
            let ok = if scause == Trap::Exception(Exception::LoadPageFault)
                || scause == Trap::Exception(Exception::FetchInstructionPageFault)
            {
                mmap_read_page_fault(vpn.into(), &mut self.page_table, area)
            } else {
                mmap_write_page_fault(vpn.into(), &mut self.page_table, area)
            };
            return ok; // false on OOM → SIGSEGV in trap handler
        }
        // brk or stack
        if let Some(area) = self
            .areas
            .iter_mut()
            .filter(|area| {
                area.area_type == MapAreaType::Brk || area.area_type == MapAreaType::Stack
            })
            .find(|area| {
                let (start, end) = area.vpn_range.range();
                start <= vpn && vpn < end
            })
        {
            return lazy_page_fault(vpn.into(), &mut self.page_table, area); // false on OOM → SIGSEGV
        }
        false
    }

    pub fn cow_page_fault(&mut self, vpn: VirtPageNum, scause: Trap) -> bool {
        // debug!("[cow_page_fault] vpn={:?}, scause={:?}", vpn, scause);
        if scause == Trap::Exception(Exception::LoadPageFault)
            || scause == Trap::Exception(Exception::FetchInstructionPageFault)
        {
            return false;
        }
        if let Some(area) = self
            .areas
            .iter_mut()
            .filter(|area| {
                area.area_type == MapAreaType::Elf
                    || area.area_type == MapAreaType::Brk
                    || area.area_type == MapAreaType::Mmap
                    || area.area_type == MapAreaType::Stack
            })
            .find(|area| {
                let (start, end) = area.vpn_range.range();
                start <= vpn && vpn < end
            })
        {
            if cow_page_fault(vpn.into(), &mut self.page_table, area) {
                return true;
            }
        }
        false
    }
}
