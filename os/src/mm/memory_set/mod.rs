//! Core [`MemorySet`] / [`MemorySetInner`] definitions and basic virtual-memory
//! operations: area insertion/removal, grow (brk), page-table access, clone, etc.
//!
//! Larger subsystems (ELF loading, fork, mmap/munmap, kernel init) live in
//! their own submodules:
//! - [`super::elf_loader`]
//! - [`super::fork_clone`]
//! - [`super::mmap_ops`]
//! - [`super::kernel_init`]

mod elf_loader;
mod fork_clone;
mod kernel_init;
mod mmap_ops;

use super::group::GROUP_SHARE;
use super::map_area::MapType;
use super::{
    read_user_bytes_direct_into, user_buffer_from_kernel, FrameTracker, MapArea, MapAreaType,
    MapPermission, PhysAddr, UserBuffer, VPNRange, VirtAddr, VirtPageNum,
};
use crate::arch::memory_layout::{KERNEL_ADDR_OFFSET, MMAP_TOP, PAGE_SIZE, USER_HEAP_SIZE};
use crate::arch::page_table::PageTable;
use crate::arch::tlb::tlb_invalidate;
use crate::fs::{File, OSFile, SEEK_CUR, SEEK_SET};
use crate::mm::PhysPageNum;
use crate::sync::SyncUnsafeCell;
use crate::syscall::MmapFlags;
use crate::trap::trap_types::*;
use crate::utils::SyscallRet;
use alloc::sync::Arc;
use alloc::vec::Vec;
use alloc::vec;
use log;
use spin::{Lazy, Mutex};

pub use elf_loader::*;
pub use fork_clone::*;
pub use kernel_init::*;
pub use mmap_ops::*;

/// a memory set instance through lazy_static! managing kernel space
pub static KERNEL_SPACE: Lazy<Mutex<MemorySetInner>> =
    Lazy::new(|| Mutex::new(MemorySetInner::new_kernel()));

const MMAP_WRITEBACK_CHUNK_SIZE: usize = 0x10000; // 64KB

pub struct MemorySet {
    pub inner: SyncUnsafeCell<MemorySetInner>,
}

impl MemorySet {
    pub fn new(memory_set: MemorySetInner) -> Self {
        Self {
            inner: SyncUnsafeCell::new(memory_set),
        }
    }
    pub fn get_mut(&self) -> &mut MemorySetInner {
        self.inner.get_unchecked_mut()
    }
    pub fn get_ref(&self) -> &MemorySetInner {
        self.inner.get_unchecked_ref()
    }
    // 对MemorySetInner封装
    #[inline(always)]
    pub fn token(&self) -> usize {
        self.inner.get_unchecked_mut().token()
    }
    #[inline(always)]
    pub fn insert_framed_area(
        &self,
        start_va: VirtAddr,
        end_va: VirtAddr,
        permission: MapPermission,
        area_type: MapAreaType,
    ) {
        self.inner
            .get_unchecked_mut()
            .insert_framed_area(start_va, end_va, permission, area_type)
    }
    #[inline(always)]
    pub fn remove_area_with_start_vpn(&self, start_vpn: VirtPageNum) {
        self.inner
            .get_unchecked_mut()
            .remove_area_with_start_vpn(start_vpn);
    }
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
        self.inner
            .get_unchecked_mut()
            .mmap(addr, len, map_perm, flags, file, off)
    }
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
    #[inline(always)]
    pub fn munmap(&self, addr: usize, len: usize) -> SyscallRet {
        self.inner.get_unchecked_mut().munmap(addr, len)
    }
    #[inline(always)]
    pub fn lazy_page_fault(&self, vpn: VirtPageNum, scause: Trap) -> bool {
        self.inner.get_unchecked_mut().lazy_page_fault(vpn, scause)
    }
    #[inline(always)]
    pub fn cow_page_fault(&self, vpn: VirtPageNum, scause: Trap) -> bool {
        self.inner.get_unchecked_mut().cow_page_fault(vpn, scause)
    }
    #[inline(always)]
    pub fn mprotect(&self, start_vpn: VirtPageNum, end_vpn: VirtPageNum, map_perm: MapPermission) {
        self.inner.get_unchecked_mut().mprotect(
            start_vpn,
            end_vpn,
            map_perm,
            None,
            usize::MAX,
            false,
        );
    }
    #[inline(always)]
    pub fn activate(&self) {
        self.inner.get_unchecked_mut().activate();
    }
    #[inline(always)]
    pub fn recycle_data_pages(&self) -> SyscallRet {
        self.inner.get_unchecked_mut().recycle_data_pages()
    }
    #[inline(always)]
    pub fn translate(&self, vpn: VirtPageNum) -> Option<PhysPageNum> {
        self.inner.get_unchecked_mut().translate(vpn)
    }
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
    #[inline(always)]
    pub fn clone_area(&self, start_vpn: VirtPageNum, another: &MemorySetInner) {
        self.get_mut().clone_area(start_vpn, another)
    }
    #[inline(always)]
    pub fn lazy_clone_area(&self, start_vpn: VirtPageNum, another: &MemorySetInner) {
        self.get_mut().lazy_clone_area(start_vpn, another)
    }
    pub fn translate_va(&self, va: VirtAddr) -> Option<PhysAddr> {
        self.get_mut().page_table.translate_va(va)
    }
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

        unsafe {
            self.inner
                .get()
                .as_ref() // 变成 Option<&MemorySetInner>
                .unwrap() // 假设你确定指针不为空
                .check_user_range(VPNRange::new(start_vpn, end_vpn), wanted_perm)
        }
    }
}

/// memory set structure, controls virtual-memory space
/// 地址空间
pub struct MemorySetInner {
    pub page_table: PageTable,
    pub areas: Vec<MapArea>,
    /// Total virtual memory allocated via mmap (bytes).
    /// Used to enforce a limit and avoid runaway allocation
    /// from exhausting physical memory through lazy page faults.
    pub total_mmap_size: usize,
}

impl MemorySetInner {
    ///Create an empty `MemorySet`
    pub fn new_bare() -> Self {
        Self {
            page_table: PageTable::new(),
            areas: Vec::new(),
            total_mmap_size: 0,
        }
    }
    pub fn new_from_kernel() -> Self {
        Self {
            page_table: PageTable::new_from_kernel(),
            areas: Vec::new(),
            total_mmap_size: 0,
        }
    }
    ///Get pagetable `root_ppn`
    pub fn token(&self) -> usize {
        self.page_table.token()
    }
    pub fn page_table_mut(self: &mut MemorySetInner) -> &mut PageTable {
        &mut self.page_table
    }
    /// Assume that no conflicts.
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
        ).ok(); // OOM is unlikely here; if it happens the area is simply not mapped
    }
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
    ///Remove `MapArea` that starts with `start_vpn`
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
    // 根据hint插入页面到指定的area并返回(va_bottom,va_top)
    // hint指示的区域必须存在
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

    // 试图找到一个插入位置，且存在提示

    // TODO: 这个函数的返回值可以试着改为VA
    // TODO: 危险的尾递归！
    pub fn find_insert_addr(&self, hint: usize, size: usize) -> usize {
        // 对hint(va)向下取整，得到hint所在的虚拟页号
        let end_vpn = VirtAddr::from(hint).floor();

        // 取hint-size所在的虚拟页号
        let start_vpn = VirtAddr::from(hint - size).floor();

        // 遍历自己的所有area，如果存在一个area完全覆盖率这个区域
        // 则把hint改为start_vpn所在位置减去一个PageSize

        for area in self.areas.iter() {
            let (start, end) = area.vpn_range.range();
            if end_vpn > start && start_vpn < end {
                let new_hint = VirtAddr::from(start_vpn).0 - PAGE_SIZE;
                return self.find_insert_addr(new_hint, size);
            }
        }
        VirtAddr::from(start_vpn).0 // 这一步是先把VPN转化为VA，然后再取值
    }

    // HXC: 为了取消task对SimpleRange的依赖，这里添加一个方法
    pub fn find_area_by_range(&mut self, l: VirtPageNum, r: VirtPageNum) -> Option<&mut MapArea> {
        let target = (l, r);
        self.areas
            .iter_mut()
            .find(|area| area.vpn_range.range() == target)
    }

    // HXC: 再加一个
    // growproc功能在MemorySet模块内的部分
    // 返回值是新的heappoint
    pub fn grow(
        &mut self,
        grow_size: isize,
        user_heappoint: usize,
        user_heapbottom: usize,
    ) -> usize {
        //因为Brk一定连续，所以就只需要有一个Brk段
        let area = self
            .areas
            .iter_mut()
            .find(|area| area.area_type == MapAreaType::Brk)
            .unwrap();
        let new_addr: usize = user_heappoint + grow_size as usize; // 生长后的地址
         let new_vpn: VirtPageNum = VirtAddr::from(new_addr).ceil();
        if grow_size > 0 {
            let user_vpn_top: VirtPageNum = ((user_heapbottom + USER_HEAP_SIZE) / PAGE_SIZE).into();
            if new_vpn >= user_vpn_top {
                panic!("USER_HEAP overflow as {:#X}!", new_addr);
            }
            //因为是懒分配，只要改范围就行了
            area.vpn_range = VPNRange::new((user_heapbottom / PAGE_SIZE).into(), new_vpn);
        } else {
            if new_addr < user_heapbottom {
                panic!("USER_HEAP downflow at {:#X}!", new_addr);
            }
            area.vpn_range = VPNRange::new((user_heapbottom / PAGE_SIZE).into(), new_vpn);
            while !area.data_frames.is_empty() {
                let page = area.data_frames.pop_last().unwrap();
                if page.0 < new_vpn {
                    area.data_frames.insert(page.0, page.1);
                    break;
                }
                self.page_table.unmap(page.0);
            }
        }
        tlb_invalidate();
        return new_addr;
    }

    /// 复制逻辑段内容
    pub fn clone_area(&mut self, start_vpn: VirtPageNum, another: &MemorySetInner) {
        if let Some(area) = another
            .areas
            .iter()
            .find(|area| area.vpn_range.start() == start_vpn)
        {
            for vpn in area.vpn_range {
                let src_ppn = another.translate(vpn).unwrap();
                let dst_ppn = self.translate(vpn).unwrap();
                dst_ppn
                    .bytes_array_mut()
                    .copy_from_slice(src_ppn.bytes_array());
            }
        }
    }
    /// 复制懒分配的逻辑段内容
    pub fn lazy_clone_area(&mut self, start_vpn: VirtPageNum, another: &MemorySetInner) {
        let another_area = if let Some(area) = another
            .areas
            .iter()
            .find(|area| area.vpn_range.start() == start_vpn)
        {
            area
        } else {
            return;
        };
        let this_area = if let Some(area) = self
            .areas
            .iter_mut()
            .find(|area| area.vpn_range.start() == start_vpn)
        {
            area
        } else {
            return;
        };
        let mut this_page_table = PageTable::from_token(self.page_table.token());
        let another_page_table = PageTable::from_token(another.page_table.token());
        for vpn in another_area.vpn_range {
            let src_ppn = match another_page_table.translate(vpn) {
                Some(ppn) => ppn,
                None => {
                    continue;
                }
            };

            let dst_ppn = match this_page_table.translate(vpn) {
                Some(ppn) => ppn,
                None => match this_area.map_one(&mut this_page_table, vpn) {
                    Some(ppn) => ppn,
                    None => continue, // OOM — skip this page
                },
            };

            dst_ppn
                .bytes_array_mut()
                .copy_from_slice(src_ppn.bytes_array());
        }
    }
    /// Push a MapArea with eager frame allocation. Returns Err(()) on OOM.
    pub(crate) fn push(&mut self, mut map_area: MapArea, data: Option<&[u8]>) -> Result<(), ()> {
        map_area.map(&mut self.page_table)?;
        if let Some(data) = data {
            map_area.copy_data(&mut self.page_table, data, 0);
        }
        self.areas.push(map_area);
        Ok(())
    }
    /// Push a MapArea with eager frame allocation and data offset. Returns Err(()) on OOM.
    pub(crate) fn push_with_offset(&mut self, mut map_area: MapArea, offset: usize, data: Option<&[u8]>) -> Result<(), ()> {
        map_area.map(&mut self.page_table)?;
        if let Some(data) = data {
            map_area.copy_data(&mut self.page_table, data, offset);
        }
        self.areas.push(map_area);
        Ok(())
    }
    pub(crate) fn push_with_given_frames(&mut self, mut map_area: MapArea, frames: Vec<Arc<FrameTracker>>) {
        map_area.map_given_frames(&mut self.page_table, frames);
        self.areas.push(map_area);
    }
    /// `push_lazily` — add a `MapArea` without immediately mapping its pages.
    /// Allocated pages are faulted in later (lazy allocation).
    pub fn push_lazily(&mut self, map_area: MapArea) {
        self.areas.push(map_area);
    }
    ///Refresh TLB with `sfence.vma`
    pub fn activate(&self) {
        self.page_table.activate();
    }
    ///Translate throuth pagetable
    pub fn translate(&self, vpn: VirtPageNum) -> Option<PhysPageNum> {
        self.page_table.translate(vpn)
    }
    ///Remove all `MapArea`
    pub fn recycle_data_pages(&mut self) -> SyscallRet {
        // 先检测是否需要munmap
        for area in self.areas.iter_mut() {
            if area.area_type == MapAreaType::Mmap {
                if area.mmap_flags.contains(MmapFlags::MAP_SHARED)
                    && area.map_perm.contains(MapPermission::W)
                {
                    if let Some(file) = area.mmap_file.file.clone() {
                        let addr: VirtAddr = area.vpn_range.start().into();
                        let mapped_len: usize = area
                            .vpn_range
                            .into_iter()
                            .filter(|vpn| area.data_frames.contains_key(&vpn))
                            .count()
                            * PAGE_SIZE;
                        let off = file.lseek(0, SEEK_CUR)?;
                        let mut written = 0;
                        while written < mapped_len {
                            let chunk_len = MMAP_WRITEBACK_CHUNK_SIZE.min(mapped_len - written);
                            let mut kernel_buf = vec![0u8; chunk_len];
                            if read_user_bytes_direct_into(
                                self.page_table.token(),
                                addr.0 as usize + written,
                                &mut kernel_buf,
                            )
                            .is_none()
                            {
                                break;
                            }
                            let buf = unsafe { user_buffer_from_kernel(&mut kernel_buf) };
                            file.lseek((area.mmap_file.offset + written) as isize, SEEK_SET)?;
                            let ret = file.write(buf)?;
                            if ret == 0 {
                                break;
                            }
                            written += ret;
                        }
                        file.lseek(off as isize, SEEK_SET)?;
                    }
                }
            }
        }
        self.areas.clear();
        self.page_table.clear();
        Ok(0)
    }
    /// 检查页表映射关系
    /// vpn_range: 待检查的范围
    /// wanted_map_perm: 想要的映射权限
    fn check_user_range(&self, vpn_range: VPNRange, wanted_map_perm: MapPermission) -> bool {
        log::trace!("[check_valid_user_vpn_range]");
        let mut current_vpn = vpn_range.start();
        let end_vpn = vpn_range.end();

        for area in self.areas.iter() {
            // 如果该区域在 current_vpn 之后，跳过
            if area.vpn_range.end() <= current_vpn {
                continue;
            }
            // 如果该区域不覆盖 current_vpn，说明有空洞
            if !area.vpn_range.contains_vpn(current_vpn) {
                log::error!(
                    "[check_valid_user_vpn_range] can't find area with vpn {:#x}",
                    current_vpn.0
                );
                self.areas.iter().for_each(|area| {
                    log::error!(
                        "[check_valid_user_vpn_range] area: {:#x?}, {:?}",
                        area.vpn_range,
                        area.map_perm
                    );
                });
                // return Err(Errno::EFAULT);
                return false;
            }
            // 权限不满足
            if !area.map_perm.contains(wanted_map_perm) {
                log::error!(
                "[check_valid_user_vpn_range] vpn {:#x} has wrong map permission: {:?}, wanted: {:?}",
                current_vpn.0,
                area.map_perm,
                wanted_map_perm
            );
                // return Err(Errno::EFAULT);
                return false;
            }
            // 更新 current_vpn 到该区域结束（不要超过 end_vpn）
            current_vpn = core::cmp::min(area.vpn_range.end(), end_vpn);

            if current_vpn >= end_vpn {
                break;
            }
        }

        if current_vpn < end_vpn {
            log::error!(
                "[check_valid_user_vpn_range] reach end prematurely at {:#x}, want {:#x}",
                current_vpn.0,
                end_vpn.0
            );
            // return Err(Errno::EFAULT);
            return false;
        }
        true
    }
}

#[allow(unused)]
#[cfg(target_arch = "riscv64")]
/// Check PageTable running correctly
pub fn remap_test() {
    // Defined in kernel_init.rs
    kernel_init::remap_test();
}

#[allow(unused)]
#[cfg(target_arch = "loongarch64")]
pub fn remap_test() {
    kernel_init::remap_test();
}
