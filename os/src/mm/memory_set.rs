//! Implementation of [`MapArea`] and [`MemorySet`].
use super::group::GROUP_SHARE;
use super::map_area::MapType;
use super::page_fault_handler::{
    cow_page_fault, lazy_page_fault, mmap_read_page_fault, mmap_write_page_fault,
};
use super::{
    translated_byte_buffer, FrameTracker, MapArea, MapAreaType, MapPermission, PhysAddr,
    UserBuffer, VPNRange, VirtAddr, VirtPageNum,
};
use crate::arch::memory_layout::MMIO_MAP_OFFSET;
use crate::arch::page_table::PageTable;
use crate::arch::tlb::tlb_invalidate;
use crate::mm::{memory_set, PhysPageNum};
use crate::trap::trap_types::*;
use crate::{
    arch::memory_layout::{
        DL_INTERP_OFFSET, KERNEL_ADDR_OFFSET, MEMORY_END, MMAP_TOP, MMIO, PAGE_SIZE, USER_HEAP_SIZE,
    },
    fs::{
        map_dynamic_link_file, map_dynamic_link_file_directly_map, open, File, OSFile, OpenFlags,
        NONE_MODE, SEEK_CUR, SEEK_SET,
    },
    sync::SyncUnsafeCell,
    syscall::MmapFlags,
    task::{Aux, AuxType},
    utils::SyscallRet,
};
use alloc::{
    string::{String, ToString},
    sync::Arc,
    vec::Vec,
};
use core::arch::asm;
use log::{debug, warn};
use spin::{Lazy, Mutex};
use xmas_elf::ElfFile;

extern "C" {
    fn stext();
    fn etext();
    fn srodata();
    fn erodata();
    fn sdata();
    fn edata();
    fn sbss_with_stack();
    fn ebss();
    fn ekernel();
    fn sigreturn_trampoline();
}

/// a memory set instance through lazy_static! managing kernel space
pub static KERNEL_SPACE: Lazy<Mutex<MemorySetInner>> =
    Lazy::new(|| Mutex::new(MemorySetInner::new_kernel()));

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
}

impl MemorySetInner {
    ///Create an empty `MemorySet`
    pub fn new_bare() -> Self {
        Self {
            page_table: PageTable::new(),
            areas: Vec::new(),
        }
    }
    pub fn new_from_kernel() -> Self {
        Self {
            page_table: PageTable::new_from_kernel(),
            areas: Vec::new(),
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
        );
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
        let new_vpn: VirtPageNum = (new_addr / PAGE_SIZE + 1).into();

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
                None => this_area.map_one(&mut this_page_table, vpn),
            };

            dst_ppn
                .bytes_array_mut()
                .copy_from_slice(src_ppn.bytes_array());
        }
    }
    fn load_dl_interp_if_needed(&mut self, elf: &ElfFile) -> Option<usize> {
        let elf_header = elf.header;
        let ph_count = elf_header.pt2.ph_count();

        let mut is_dl = false;
        for i in 0..ph_count {
            let ph = elf.program_header(i).unwrap();
            if ph.get_type().unwrap() == xmas_elf::program::Type::Interp {
                is_dl = true;
                break;
            }
        }

        if is_dl {
            debug!("[load_dl] encounter a dl elf");
            let section = elf.find_section_by_name(".interp").unwrap();
            let mut interp = String::from_utf8(section.raw_data(&elf).to_vec()).unwrap();
            interp = interp.strip_suffix("\0").unwrap_or(&interp).to_string();
            debug!("[load_dl] interp {}", interp);

            let interp = map_dynamic_link_file_directly_map(&interp);

            let interp_inode = open(&interp, OpenFlags::O_RDONLY, NONE_MODE)
                .unwrap()
                .file()
                .ok();
            let interp_file = interp_inode.unwrap();
            let interp_elf_data = interp_file.inode.read_all().unwrap();
            let interp_elf = xmas_elf::ElfFile::new(&interp_elf_data).unwrap();
            self.map_elf(&interp_elf, DL_INTERP_OFFSET.into());

            Some(interp_elf.header.pt2.entry_point() as usize + DL_INTERP_OFFSET)
        } else {
            debug!("[load_dl] encounter a static elf");
            None
        }
    }
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
        // 映射到固定地址
        // 如果已经映射的部分和需要固定映射的部分冲突,已经映射的部分将被拆分
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
                    VirtAddr::from(addr),
                    VirtAddr::from(addr + len),
                    MapType::Framed,
                    map_perm,
                    MapAreaType::Mmap,
                    file,
                    off,
                    flags,
                ));
            }
            return addr;
        }
        // 自行选择地址,计算已经使用的MMap地址
        let addr = self.find_insert_addr(MMAP_TOP, len);
        debug!(
            "[sys_mmap] start_va:{:#x},end_va:{:#x}",
            VirtAddr::from(addr).0,
            VirtAddr::from(addr + len).0
        );
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
        addr
        // addr
    }
    /// munmap
    pub fn munmap(&mut self, addr: usize, len: usize) -> SyscallRet {
        let start_vpn = VirtPageNum::from(VirtAddr::from(addr));
        let end_vpn = VirtPageNum::from(VirtAddr::from(addr + len));
        // debug!(
        //     "[MemorySet] start_vpn:{:#x},end_vpn:{:#x}",
        //     start_vpn.0, end_vpn.0
        // );
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
            // 检查是否需要写回
            if area.mmap_flags.contains(MmapFlags::MAP_SHARED)
                && area.map_perm.contains(MapPermission::W)
            {
                // 相邻的页面一次写回
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
                // 每次写回前要设置偏移量
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
            // debug!(
            //     "[area vpn_range] start:{:#x},end:{:#x}",
            //     area.vpn_range.start().0,
            //     area.vpn_range.end().0
            // );
            // 取消映射
            for vpn in VPNRange::new(start_vpn, end_vpn) {
                area.unmap_one(&mut self.page_table, vpn);
            }
            let area_end_vpn = area.vpn_range.end();
            // debug!(
            //     "[MemorySet] end_vpn:{:#x},area_end_vpn:{:#x}",
            //     end_vpn.0, area_end_vpn.0
            // );
            // 是否回收,mprotect可能将mmap区域拆分成多个
            if area_end_vpn <= end_vpn {
                self.areas.remove(idx);
            } else {
                area.vpn_range = VPNRange::new(end_vpn, area_end_vpn);
            }
            tlb_invalidate();
        }
        Ok(0)
    }
    pub fn lazy_page_fault(&mut self, vpn: VirtPageNum, scause: Trap) -> bool {
        // TODO:
        // 1. 大多数情况下，本函数的调用者已经检查过ppn是否为None；所以下面的检查大多数是多余的，能不能优化
        // 2. 本函数能不能返回ppn，这样调用者就不需要再调用一次translate了
        let ppn = self.page_table.translate(vpn);
        if !ppn.is_none() {
            return false;
        }
        //mmap
        if let Some(area) = self
            .areas
            .iter_mut()
            .filter(|area| area.area_type == MapAreaType::Mmap)
            .find(|area| {
                let (start, end) = area.vpn_range.range();
                start <= vpn && vpn < end
            })
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
        //brk or stack
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
            lazy_page_fault(vpn.into(), &mut self.page_table, area);
            return true;
        }
        false
    }
    pub fn cow_page_fault(&mut self, vpn: VirtPageNum, scause: Trap) -> bool {
        if scause == Trap::Exception(Exception::LoadPageFault)
            || scause == Trap::Exception(Exception::FetchInstructionPageFault)
        {
            return false;
        }
        //找到触发cow的段
        if let Some(area) = self
            .areas
            .iter_mut()
            .filter(|area| {
                area.area_type == MapAreaType::Elf
                    || area.area_type == MapAreaType::Brk
                    || area.area_type == MapAreaType::Mmap
            })
            .find(|area| {
                let (start, end) = area.vpn_range.range();
                start <= vpn && vpn < end
            })
        {
            // if let Some(pte_flags) = self.page_table.get_pte_flags(vpn) {
            //     if pte_flags.contains(PTEFlags::COW) {
            //         cow_page_fault(vpn.into(), &mut self.page_table, area);
            //     }
            //     return true; // TODO ERROR 这个return true似乎应该放在上面的if里面，未验证
            // }
            if cow_page_fault(vpn.into(), &mut self.page_table, area) {
                return true;
            }
        }
        false
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
        //因修改而新增的Area
        let mut new_areas = Vec::new();
        for area in self.areas.iter_mut() {
            let (start, end) = area.vpn_range.range();
            if start >= start_vpn && end <= end_vpn {
                //修改整个area
                area.map_perm = map_perm;
                if if_mmap {
                    area.mmap_file.file = file.clone();
                }
                if offset != usize::MAX {
                    area.mmap_file.offset = offset as usize;
                }
                continue;
            } else if start < start_vpn && end > start_vpn && end <= end_vpn {
                //修改area后半部分
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
                //修改area前半部分
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
                //修改area中间部分
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
            //剩下的情况无相交部分，无需修改
        }
        for area in new_areas {
            self.areas.push(area);
        }
        // let pte_flags = PTEFlags::from_bits(map_perm.bits() as usize).unwrap();

        for vpn in start_vpn.0..=end_vpn.0 {
            /* ERROR TODO:
            这里的逻辑有问题，应该修改为：
                只修改已映射页面的PTE：对于已经有物理页映射的虚拟页，直接修改其PTE权限
                在MapArea中记录权限：对于未映射的页面，将新权限保存在对应的MapArea中
                懒分配时应用权限：当这些未映射的页面在后续被访问而触发页面错误时，使用MapArea中保存的权限来分配页面
            */
            // 保险期间，使用老表达
            // let pte = self.page_table.find_pte_create(vpn.into()).unwrap();
            // let old_flags = pte.get_flags();
            // pte.set_flags(pte_flags | old_flags);
            self.page_table.handle_mprotect(vpn.into(), map_perm); // 这个函数和以上注释掉的三行逻辑一致
        }
        tlb_invalidate();
    }
    fn push(&mut self, mut map_area: MapArea, data: Option<&[u8]>) {
        map_area.map(&mut self.page_table);
        if let Some(data) = data {
            map_area.copy_data(&mut self.page_table, data, 0);
        }
        self.areas.push(map_area);
    }
    fn push_with_offset(&mut self, mut map_area: MapArea, offset: usize, data: Option<&[u8]>) {
        map_area.map(&mut self.page_table);
        if let Some(data) = data {
            map_area.copy_data(&mut self.page_table, data, offset);
        }
        self.areas.push(map_area);
    }
    fn push_with_given_frames(&mut self, mut map_area: MapArea, frames: Vec<Arc<FrameTracker>>) {
        map_area.map_given_frames(&mut self.page_table, frames);
        self.areas.push(map_area);
    }
    /// 不映射MapArea里的虚拟页面
    pub fn push_lazily(&mut self, map_area: MapArea) {
        self.areas.push(map_area);
    }

    ///仅initproc会用，将懒分配的全部分配
    // fn unlazy(&mut self) {
    //     for map_area in self.areas.iter_mut() {
    //         map_area.map(&mut self.page_table);
    //     }
    // }
    /// Without kernel stacks.
    #[cfg(target_arch = "riscv64")]
    pub fn new_kernel() -> Self {
        let mut memory_set = Self::new_bare();
        println!("kernel token: {:#x}", memory_set.page_table.token());
        println!(
            ".text [{:#x}, {:#x})",
            stext as *const () as usize, etext as *const () as usize
        );
        println!(
            ".rodata [{:#x}, {:#x})",
            srodata as *const () as usize, erodata as *const () as usize
        );
        println!(
            ".data [{:#x}, {:#x})",
            sdata as *const () as usize, edata as *const () as usize
        );
        println!(
            ".bss [{:#x}, {:#x})",
            sbss_with_stack as *const () as usize, ebss as *const () as usize
        );
        println!(
            "sigreturn_trampoline start: [{:#x}, {:#x}",
            sigreturn_trampoline as *const () as usize,
            sigreturn_trampoline as *const () as usize + PAGE_SIZE
        );
        // map kernel sections
        println!("mapping .text section");
        let s_sig_trap = sigreturn_trampoline as *const () as usize;
        let e_sig_trap = sigreturn_trampoline as *const () as usize + PAGE_SIZE;
        memory_set.push(
            MapArea::new(
                (stext as *const () as usize).into(),
                (s_sig_trap).into(),
                MapType::Direct,
                MapPermission::R | MapPermission::X,
                MapAreaType::Elf,
            ),
            None,
        );
        memory_set.push(
            MapArea::new(
                (e_sig_trap).into(),
                (etext as *const () as usize).into(),
                MapType::Direct,
                MapPermission::R | MapPermission::X,
                MapAreaType::Elf,
            ),
            None,
        );
        memory_set.push(
            MapArea::new(
                (s_sig_trap).into(),
                (e_sig_trap).into(),
                MapType::Direct,
                MapPermission::R | MapPermission::X | MapPermission::U,
                MapAreaType::Elf,
            ),
            None,
        );
        println!("mapping .rodata section");
        memory_set.push(
            MapArea::new(
                (srodata as *const () as usize).into(),
                (erodata as *const () as usize).into(),
                MapType::Direct,
                MapPermission::R,
                MapAreaType::Elf,
            ),
            None,
        );
        println!("mapping .data section");
        memory_set.push(
            MapArea::new(
                (sdata as *const () as usize).into(),
                (edata as *const () as usize).into(),
                MapType::Direct,
                MapPermission::R | MapPermission::W,
                MapAreaType::Elf,
            ),
            None,
        );
        println!("mapping .bss section");
        memory_set.push(
            MapArea::new(
                (sbss_with_stack as *const () as usize).into(),
                (ebss as *const () as usize).into(),
                MapType::Direct,
                MapPermission::R | MapPermission::W,
                MapAreaType::Elf,
            ),
            None,
        );
        println!("mapping physical memory");
        memory_set.push(
            MapArea::new(
                (ekernel as *const () as usize).into(),
                MEMORY_END.into(),
                MapType::Direct,
                MapPermission::R | MapPermission::W,
                MapAreaType::Physical,
            ),
            None,
        );
        println!("mapping memory-mapped registers");
        for pair in MMIO {
            let start_va = (*pair).0 + MMIO_MAP_OFFSET;
            let end_va = start_va + (*pair).1;
            memory_set.push(
                MapArea::new(
                    start_va.into(),
                    end_va.into(),
                    MapType::Direct,
                    MapPermission::R | MapPermission::W,
                    MapAreaType::MMIO,
                ),
                None,
            );
        }
        println!("create new kernel successfully!");
        memory_set
    }

    #[cfg(target_arch = "loongarch64")]
    pub fn new_kernel() -> Self {
        let memory_set = Self::new_bare();
        println!("kernel token: {:#x}", memory_set.page_table.token());
        println!(".text [{:#x}, {:#x})", stext as usize, etext as usize);
        println!(".rodata [{:#x}, {:#x})", srodata as usize, erodata as usize);
        println!(".data [{:#x}, {:#x})", sdata as usize, edata as usize);
        println!(
            ".bss [{:#x}, {:#x})",
            sbss_with_stack as usize, ebss as usize
        );
        println!(
            "sigreturn_trampoline start: [{:#x}, {:#x}",
            sigreturn_trampoline as usize,
            sigreturn_trampoline as usize + PAGE_SIZE
        );

        // println!("mapping memory-mapped registers");
        // for pair in MMIO {
        //     let start_va = (*pair).0 + MMIO_MAP_OFFSET;
        //     let end_va = start_va + (*pair).1;
        //     println!("map [{:#x}, {:#x}]", start_va, end_va);
        //     memory_set.push(
        //         MapArea::new(
        //             start_va.into(),
        //             end_va.into(),
        //             MapType::Direct,
        //             MapPermission::R | MapPermission::W,
        //             MapAreaType::MMIO,
        //         ),
        //         None,
        //     );
        //     for va in (start_va..end_va).step_by(PAGE_SIZE) {
        //         let va = VirtAddr::from(va);
        //         let pte = memory_set.page_table.translate(va.floor());
        //         if let Some(pte) = pte {
        //             pte.set_uncached();
        //         } else {
        //             panic!("cannot find pte");
        //         }
        //     }
        // }
        println!("create new kernel successfully!");
        memory_set
    }
    /// Include sections in elf and trampoline and TrapContext and user stack,
    /// also returns user_sp and entry point.
    /// 不包括用户栈和Trap
    pub fn from_elf(elf_data: &[u8]) -> (Self, usize, usize, Vec<Aux>) {
        let mut auxv = Vec::new();
        let mut memory_set = Self::new_from_kernel();
        // debug!("from_elf new stap={:#x}", memory_set.page_table.token());
        // map program headers of elf, with U flag
        let elf = xmas_elf::ElfFile::new(elf_data).unwrap();
        let elf_header = elf.header;
        let magic = elf_header.pt1.magic;
        assert_eq!(magic, [0x7f, 0x45, 0x4c, 0x46], "invalid elf!");
        let ph_count = elf_header.pt2.ph_count();
        // let mut head_va = 0; // top va of ELF which points to ELF header
        let mut entry_point = elf.header.pt2.entry_point() as usize;

        auxv.push(Aux::new(
            AuxType::PHENT,
            elf.header.pt2.ph_entry_size() as usize,
        )); // ELF64 header 64bytes
        auxv.push(Aux::new(AuxType::PHNUM, ph_count as usize));
        auxv.push(Aux::new(AuxType::PAGESZ, PAGE_SIZE as usize));
        // 设置动态链接
        if let Some(interp_entry_point) = memory_set.load_dl_interp_if_needed(&elf) {
            auxv.push(Aux::new(AuxType::BASE, DL_INTERP_OFFSET));
            entry_point = interp_entry_point;
        } else {
            auxv.push(Aux::new(AuxType::BASE, 0));
        }
        auxv.push(Aux::new(AuxType::FLAGS, 0 as usize));
        auxv.push(Aux::new(
            AuxType::ENTRY,
            elf.header.pt2.entry_point() as usize,
        ));
        auxv.push(Aux::new(AuxType::UID, 0 as usize));
        auxv.push(Aux::new(AuxType::EUID, 0 as usize));
        auxv.push(Aux::new(AuxType::GID, 0 as usize));
        auxv.push(Aux::new(AuxType::EGID, 0 as usize));
        auxv.push(Aux::new(AuxType::PLATFORM, 0 as usize));
        auxv.push(Aux::new(AuxType::HWCAP, 0 as usize));
        auxv.push(Aux::new(AuxType::CLKTCK, 100 as usize));
        auxv.push(Aux::new(AuxType::SECURE, 0 as usize));
        auxv.push(Aux::new(AuxType::NOTELF, 0x112d as usize));

        let (max_end_vpn, head_va) = memory_set.map_elf(&elf, VirtAddr(0));

        // Get ph_head addr for auxv
        let ph_head_addr = head_va.0 + elf.header.pt2.ph_offset() as usize;
        auxv.push(Aux {
            aux_type: AuxType::PHDR,
            value: ph_head_addr as usize,
        });
        //map user heap
        let max_end_va: VirtAddr = max_end_vpn.into();
        let mut user_heap_bottom: usize = max_end_va.into();
        //guard page
        user_heap_bottom += PAGE_SIZE;
        let user_heap_top: usize = user_heap_bottom;
        memory_set.push_lazily(MapArea::new(
            user_heap_bottom.into(),
            user_heap_top.into(),
            MapType::Framed,
            MapPermission::R | MapPermission::W | MapPermission::U,
            MapAreaType::Brk,
        ));

        // println!("start:{:#X}", elf.header.pt2.entry_point() as usize);
        (memory_set, user_heap_bottom, entry_point, auxv)
    }
    fn map_elf(&mut self, elf: &ElfFile, offset: VirtAddr) -> (VirtPageNum, VirtAddr) {
        let elf_header = elf.header;
        let ph_count = elf_header.pt2.ph_count();

        let mut max_end_vpn = offset.floor();
        let mut header_va = 0;
        let mut has_found_header_va = false;

        for i in 0..ph_count {
            let ph = elf.program_header(i).unwrap();
            if ph.get_type().unwrap() == xmas_elf::program::Type::Load {
                let start_va: VirtAddr = (ph.virtual_addr() as usize + offset.0).into();
                let end_va: VirtAddr =
                    ((ph.virtual_addr() + ph.mem_size()) as usize + offset.0).into();
                if !has_found_header_va {
                    header_va = start_va.0;
                    has_found_header_va = true;
                }
                let mut map_perm = MapPermission::U;
                let ph_flags = ph.flags();
                if ph_flags.is_read() {
                    map_perm |= MapPermission::R;
                }
                if ph_flags.is_write() {
                    map_perm |= MapPermission::W;
                }
                if ph_flags.is_execute() {
                    map_perm |= MapPermission::X;
                }
                let map_area = MapArea::new(
                    start_va,
                    end_va,
                    MapType::Framed,
                    map_perm,
                    MapAreaType::Elf,
                );
                let data_offset = start_va.0 - start_va.floor().0 * PAGE_SIZE;
                max_end_vpn = map_area.vpn_range.end();
                self.push_with_offset(
                    map_area,
                    data_offset,
                    Some(&elf.input[ph.offset() as usize..(ph.offset() + ph.file_size()) as usize]),
                );
            }
        }
        (max_end_vpn, header_va.into())
    }
    ///Clone a same `MemorySet`
    pub fn from_existed_user(user_space: &MemorySet) -> MemorySetInner {
        let mut memory_set = Self::new_from_kernel();
        // copy data sections
        for area in user_space.get_mut().areas.iter_mut() {
            // don't copy stack and trap
            // 每个线程单独分配
            if area.area_type == MapAreaType::Stack || area.area_type == MapAreaType::Trap {
                continue;
            }
            let mut new_area = MapArea::from_another(area);
            if area.area_type == MapAreaType::Mmap
                && !area.mmap_flags.contains(MmapFlags::MAP_SHARED)
            {
                GROUP_SHARE.lock().add_area(new_area.groupid);
            }
            // Mmap和brk是lazy allocation
            if area.area_type == MapAreaType::Mmap || area.area_type == MapAreaType::Brk {
                //已经分配且独占/被写过的部分以及读共享部分按cow处理
                //其余是未分配部分，直接clone即可
                if area.mmap_flags.contains(MmapFlags::MAP_SHARED) {
                    let frames = area.data_frames.values().cloned().collect();
                    memory_set.push_with_given_frames(new_area, frames);
                    continue;
                }
                new_area.data_frames = area.data_frames.clone();
                for (vpn, _) in area.data_frames.iter() {
                    let vpn = *vpn;
                    // let pte = user_space.get_mut().page_table.translate(vpn).unwrap();
                    // let mut pte_flags = pte.get_flags();
                    // let src_ppn = pte.get_ppn();
                    // // 对于可写的页，或者有写时复制的标志位的页
                    // // 需要考虑写时复制
                    // if pte_flags.contains(PTEFlags::WRITEABLE) || pte_flags.contains(PTEFlags::COW)
                    // {
                    //     pte_flags &= !PTEFlags::WRITEABLE;
                    //     pte_flags |= PTEFlags::COW;
                    // }

                    // pte.set_flags(pte_flags);
                    // memory_set.page_table.map(vpn, src_ppn, pte_flags);
                    user_space
                        .get_mut()
                        .page_table
                        .handle_cow_mapping_from_exited_user(vpn, &mut memory_set);
                }
                memory_set.push_lazily(new_area);
                continue;
            }
            // let mut page_table = &mut user_space.page_table;
            // ELF总是cow的
            if area.area_type == MapAreaType::Elf {
                for vpn in area.vpn_range {
                    // 此段逻辑和上面很类似，可以考虑合并
                    // let pte = user_space.get_mut().page_table.translate(vpn).unwrap();
                    // let pte_flags = (pte.get_flags() & !PTEFlags::WRITEABLE) | PTEFlags::COW;
                    // let src_ppn = pte.get_ppn();
                    // pte.set_flags(pte_flags);
                    // memory_set.page_table.map(vpn, src_ppn, pte_flags);
                    user_space
                        .get_mut()
                        .page_table
                        .handle_cow_mapping_from_exited_user(vpn, &mut memory_set);
                }

                new_area.data_frames = area.data_frames.clone();
                memory_set.push_lazily(new_area);
                continue;
            }
            // 映射相同的Frame
            if area.area_type == MapAreaType::Shm {
                let frames = area.data_frames.values().cloned().collect();
                memory_set.push_with_given_frames(new_area, frames);
                continue;
            }

            //既不是cow也不是mmap还不是shm
            memory_set.push(new_area, None);

            // copy data from another space
            for vpn in area.vpn_range {
                let src_ppn = user_space.translate(vpn).unwrap();
                let dst_ppn = memory_set.translate(vpn).unwrap();
                dst_ppn
                    .bytes_array_mut()
                    .copy_from_slice(src_ppn.bytes_array_mut());
            }
        }
        tlb_invalidate();
        memory_set
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
                    let addr: VirtAddr = area.vpn_range.start().into();
                    let mapped_len: usize = area
                        .vpn_range
                        .into_iter()
                        .filter(|vpn| area.data_frames.contains_key(&vpn))
                        .count()
                        * PAGE_SIZE;
                    let file = area.mmap_file.file.clone().unwrap();
                    file.write(UserBuffer {
                        buffers: translated_byte_buffer(
                            self.page_table.token(),
                            addr.0 as *const u8,
                            mapped_len,
                        )
                        .unwrap(),
                    })?;
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
///Check PageTable running correctly
pub fn remap_test() {
    println!("remap test start!");
    let mut kernel_space = KERNEL_SPACE.lock();
    // let mid_text: VirtAddr = (stext as usize + (etext as usize - stext as usize) / 2).into();
    // let mid_rodata: VirtAddr =
    //     (srodata as usize + (erodata as usize - srodata as usize) / 2).into();
    // let mid_data: VirtAddr = (sdata as usize + (edata as usize - sdata as usize) / 2).into();
    // assert!(!kernel_space
    //     .page_table
    //     .translate(mid_text.floor())
    //     .unwrap()
    //     .get_flags()
    //     .contains(PTEFlags::WRITEABLE));
    // assert!(!kernel_space
    //     .page_table
    //     .translate(mid_rodata.floor())
    //     .unwrap()
    //     .get_flags()
    //     .contains(PTEFlags::WRITEABLE));
    // assert!(!kernel_space
    //     .page_table
    //     .translate(mid_data.floor())
    //     .unwrap()
    //     .get_flags()
    //     .contains(PTEFlags::EXECUTABLE));
    kernel_space.page_table.handle_remap_test();
    println!("remap_test passed!");
}

#[allow(unused)]
#[cfg(target_arch = "loongarch64")]
pub fn remap_test() {
    println!("loongarch64 does not need remap_test");
}
