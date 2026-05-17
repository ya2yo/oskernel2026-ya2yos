use super::{
    memory_layout::{KERNEL_PGNUM_OFFSET, PAGE_SIZE, PAGE_SIZE_BITS},
    tlb::tlb_invalidate,
};
use crate::mm::{
    address::*, translate, FrameTracker, MapArea, MapPermission, MemorySetInner, KERNEL_SPACE,
};
use alloc::{sync::Arc, vec, vec::Vec};
use bitflags::*;
use riscv::register::satp;

use log::debug;

#[inline(always)]
pub fn get_token_from_regs() -> usize {
    satp::read().bits() & ((1 << 44) - 1)
}

// bitflags! {
//     // 页表项标志位
//     pub struct PTEFlags: usize {
//         const VALID = 1 << 0;       // 有效位
//         const READABLE = 1 << 1;    // 可读
//         const WRITEABLE = 1 << 2;   // 可写
//         const EXECUTABLE = 1 << 3;  // 可执行
//         const USER = 1 << 4;        // 用户态访问
//         const COW = 1 << 9;         // 写时复制
//     }
// }

bitflags! {
    pub struct RVPTEFlags: usize {
        const VALID = 1 << 0;
        const READABLE = 1 << 1;
        const WRITEABLE = 1 << 2;
        const EXECUTABLE = 1 << 3;
        const USER = 1 << 4;
        const GLOBAL = 1 << 5;
        const ACCESSED = 1 << 6;
        const DIRTY = 1 << 7;
        const COW = 1 << 9;
        const RESERVED_10 = 1 << 10;
    }
}

impl From<MapPermission> for RVPTEFlags {
    fn from(perm: MapPermission) -> Self {
        // TODO:是否可以通过编译检查，将这里的if判断去掉，因为MapPermission是和RVPTEFlags正好一一对应
        let mut flags = RVPTEFlags::VALID;
        if perm.contains(MapPermission::R) {
            flags.insert(RVPTEFlags::READABLE);
        }
        if perm.contains(MapPermission::W) {
            flags.insert(RVPTEFlags::WRITEABLE);
        }
        if perm.contains(MapPermission::X) {
            flags.insert(RVPTEFlags::EXECUTABLE);
        }
        if perm.contains(MapPermission::U) {
            flags.insert(RVPTEFlags::USER);
        }
        flags
    }
}

impl From<RVPTEFlags> for MapPermission {
    fn from(flags: RVPTEFlags) -> Self {
        let mut perm = MapPermission::empty();
        if flags.contains(RVPTEFlags::READABLE) {
            perm.insert(MapPermission::R);
        }
        if flags.contains(RVPTEFlags::WRITEABLE) {
            perm.insert(MapPermission::W);
        }
        if flags.contains(RVPTEFlags::EXECUTABLE) {
            perm.insert(MapPermission::X);
        }
        if flags.contains(RVPTEFlags::USER) {
            perm.insert(MapPermission::U);
        }
        perm
    }
}

// impl From<RVPTEFlags> for PTEFlags {
//     fn from(flags: RVPTEFlags) -> Self {
//         PTEFlags::from_bits_truncate(flags.bits as usize)
//     }
// }

// impl From<PTEFlags> for RVPTEFlags {
//     fn from(flags: PTEFlags) -> Self {
//         RVPTEFlags::from_bits_truncate(flags.bits as usize)
//     }
// }

#[derive(Copy, Clone)]
#[repr(C)]
/// page table entry structure
struct PageTableEntry {
    ///PTE
    bits: usize,
}
impl PageTableEntry {
    fn new(ppn: PhysPageNum, flags: RVPTEFlags) -> Self {
        PageTableEntry {
            bits: ppn.0 << 10 | flags.bits,
        }
    }
    #[inline(always)]
    fn get_ppn(&self) -> PhysPageNum {
        (self.bits >> 10 & ((1usize << 44) - 1)).into()
    }
    #[inline(always)]
    fn get_flags(&self) -> RVPTEFlags {
        // 仅取低10位
        RVPTEFlags::from_bits(self.bits & (0x3FF)).unwrap()
    }
    #[inline(always)]
    fn set_flags(&mut self, flags: RVPTEFlags) {
        self.bits = (self.bits & !0x3FF) | flags.bits;
    }
}

///Record root ppn and has the same lifetime as 1 and 2 level `PageTableEntry`
pub struct PageTable {
    root_ppn: PhysPageNum,          // 根页表的地址
    frames: Vec<Arc<FrameTracker>>, // 该页表涉及到的用于存储页表的物理页
}
// 实现页表的私有方法
impl PageTable {
    /// 相当于PKE操作系统实验的page_walk的alloc=1的情况
    /// 从页表开始找到最终物理页的页表项的可变引用
    /// 在此过程中，如果中途发现2/3级页表不存在，则会开辟
    /// 但是，不会开辟最终用于存储数据的那个物理页
    fn find_pte_create(&mut self, vpn: VirtPageNum) -> Option<&mut PageTableEntry> {
        let idxs = vpn.indexes();
        let mut ppn = self.root_ppn;
        let mut result: Option<&mut PageTableEntry> = None;

        for (i, idx) in idxs.iter().enumerate() {
            let pte = &mut ppn.as_array::<PageTableEntry>()[*idx];
            if i == 2 {
                result = Some(pte);
                break;
            }
            if !pte.get_flags().contains(RVPTEFlags::VALID) {
                let frame = FrameTracker::alloc().unwrap();
                *pte = PageTableEntry::new(frame.ppn, RVPTEFlags::VALID);
                self.frames.push(frame);
            }
            ppn = pte.get_ppn();
        }
        result
    }
    /// Find phsical address by virtual address
    fn find_pte(&self, vpn: VirtPageNum) -> Option<&mut PageTableEntry> {
        let idxs = vpn.indexes();
        let mut ppn = self.root_ppn;
        let mut result: Option<&mut PageTableEntry> = None;
        for (i, idx) in idxs.iter().enumerate() {
            let pte = &mut ppn.as_array::<PageTableEntry>()[*idx];
            if i == 2 {
                result = Some(pte);
                break;
            }
            if !pte.get_flags().contains(RVPTEFlags::VALID) {
                return None;
            }
            ppn = pte.get_ppn();
        }
        result
    }
    /// return: 有效的页表项
    fn find_valid_pte(&self, vpn: VirtPageNum) -> Option<&mut PageTableEntry> {
        self.find_pte(vpn)
            .filter(|pte| pte.get_flags().contains(RVPTEFlags::VALID))
    }
    fn map_by_pte_flags(&mut self, vpn: VirtPageNum, ppn: PhysPageNum, pte_flags: RVPTEFlags) {
        let pte = self.find_pte_create(vpn).unwrap();
        assert!(
            !pte.get_flags().contains(RVPTEFlags::VALID),
            "vpn {:?}, va {:x} is mapped before mapping",
            vpn,
            vpn.0 << PAGE_SIZE_BITS
        );
        *pte = PageTableEntry::new(ppn, pte_flags | RVPTEFlags::VALID);
    }
}

impl Default for PageTable {
    fn default() -> Self {
        Self::new()
    }
}

// 实现PageTable的通用函数
impl PageTable {
    /// 生成空页表
    pub fn new() -> Self {
        let frame = FrameTracker::alloc().unwrap();
        PageTable {
            root_ppn: frame.ppn,
            frames: vec![frame],
        }
    }
    pub fn new_from_kernel() -> Self {
        let frame = FrameTracker::alloc().unwrap();
        let locked_kernel = KERNEL_SPACE.lock();
        let kernel_root_ppn = locked_kernel.page_table.root_ppn;
        // 第一级页表
        let index = VirtPageNum::from(KERNEL_PGNUM_OFFSET).indexes()[0];
        frame.ppn.as_array::<PageTableEntry>()[index..]
            .copy_from_slice(&kernel_root_ppn.as_array::<PageTableEntry>()[index..]);
        PageTable {
            root_ppn: frame.ppn,
            frames: vec![frame],
        }
    }
    pub fn activate(&self) {
        // 生成页表对应的satp寄存器的值
        // stap组成：MODE|ASID|PPN
        // MODE为8表示启用39位虚拟地址，ASDI不管（全0即可），PPN为根的物理页号
        let satp = 8usize << 60 | self.root_ppn.0;
        satp::write(satp);
        tlb_invalidate();
    }
    pub fn clear(&mut self) {
        self.frames.clear();
    }
    /// Create a mapping form `vpn` to `ppn`
    /// 该函数会自行把PTEFlags::VALID补充进flags
    /// TODO: 可以以返回值的形式报错，而非assert
    #[inline]
    pub fn map(&mut self, vpn: VirtPageNum, ppn: PhysPageNum, flags: MapPermission) {
        self.map_by_pte_flags(vpn, ppn, RVPTEFlags::from(flags));
    }
    /// Delete a mapping form `vpn`
    pub fn unmap(&mut self, vpn: VirtPageNum) {
        // 如果不存在,即lazy allocation,跳过即可
        if let Some(pte) = self.find_pte(vpn) {
            if pte.get_flags().contains(RVPTEFlags::VALID) {
                *pte = PageTableEntry { bits: 0 };
            }
        }
    }
    /// return: vpn对应的有效ppn，页表项无效和不存在返回None
    pub fn translate(&self, vpn: VirtPageNum) -> Option<PhysPageNum> {
        self.find_pte(vpn)
            .filter(|pte| pte.get_flags().contains(RVPTEFlags::VALID))
            .map(|pte| pte.get_ppn())
    }
    /// Translate `VirtAddr` to `PhysAddr`，页表项无效和不存在返回None
    pub fn translate_va(&self, va: VirtAddr) -> Option<PhysAddr> {
        let vpn = va.floor();
        self.translate(vpn).map(|ppn| {
            let aligned_pa: PhysAddr = ppn.into();
            let offset = va.page_offset();
            let aligned_pa_usize: usize = aligned_pa.into();
            (aligned_pa_usize + offset).into()
        })
    }
    pub fn token(&self) -> usize {
        self.root_ppn.0
    }
    pub fn from_token(token: usize) -> Self {
        PageTable {
            root_ppn: token.into(),
            frames: Vec::new(),
        }
    }
}
/// 实现和PageTable相关的特化函数 handle_xxx
/// 将和页表相关的函数实现到PageTable上，目的是由页表维护页表一致性，且出问题方面排查
/// 这里尽量使用上面的pub接口
impl PageTable {
    /// param: param add_flags: 需要添加的权限
    pub fn handle_mprotect(&mut self, vpn: VirtPageNum, add_flags: MapPermission) {
        let pte = self.find_pte_create(vpn).unwrap();
        let old_flags = pte.get_flags();
        pte.set_flags(RVPTEFlags::from_bits_truncate(add_flags.bits() as usize) | old_flags);
    }
    /// return: 若错误是COW且成功处理了COW页错误，返回true，否则返回false
    pub fn handle_cow_page_fault(&mut self, va: VirtAddr, vma: &mut MapArea) -> bool {
        let pte = match self.find_valid_pte(va.floor()) {
            Some(pte) => pte,
            None => return false,
        };
        let pte_flags = pte.get_flags();
        if !pte_flags.contains(RVPTEFlags::COW) {
            // 在原来的写法中，不论是否有cow标志都会返回true，这里改为没有cow标志时返回false
            // 简单测试发现此处valid的pte都具有cow标志，在这放一个panic，看看未来是否会出现panic
            panic!("ly: a valid pte without COW flag found at {:#x}", va.0);
            return false;
        }

        // 只有一个，不用复制
        let frame = vma.data_frames.get(&va.into()).unwrap();

        if Arc::strong_count(frame) == 1 {
            let mut flags = pte.get_flags();
            flags.remove(RVPTEFlags::COW);
            flags.insert(RVPTEFlags::WRITEABLE);
            pte.set_flags(flags);
            return true;
        }

        //旧物理页的内容复制到新物理页
        // 原物理页：
        let src = pte.get_ppn().bytes_array_mut();
        // 取消原来的映射，新建一个映射
        vma.unmap_one(self, va.into());
        vma.map_one(self, va.into());
        tlb_invalidate();
        // 新物理页
        let pte = self.find_valid_pte(va.floor()).unwrap();
        let dst = &mut pte.get_ppn().bytes_array_mut()[..PAGE_SIZE];
        dst.copy_from_slice(src);

        let mut flags = pte.get_flags();
        flags.remove(RVPTEFlags::COW);
        flags.insert(RVPTEFlags::WRITEABLE);
        pte.set_flags(flags);

        true
    }
    /// 处理页表项的COW映射：将可写的页转换为COW页
    pub fn handle_cow_mapping_from_exited_user(
        &self,
        vpn: VirtPageNum,
        memory_set: &mut MemorySetInner,
    ) {
        let pte = self.find_valid_pte(vpn).unwrap();
        let mut pte_flags = pte.get_flags();
        let src_ppn = pte.get_ppn();
        // 对于可写的页，或者有写时复制的标志位的页
        // 需要考虑写时复制
        if pte_flags.contains(RVPTEFlags::WRITEABLE) {
            pte_flags &= !RVPTEFlags::WRITEABLE;
            pte_flags |= RVPTEFlags::COW;
        }
        pte.set_flags(pte_flags);
        memory_set
            .page_table
            .map_by_pte_flags(vpn, src_ppn, pte_flags);
    }
    pub fn handle_remap_test(&mut self) {
        extern "C" {
            fn stext();
            fn etext();
            fn srodata();
            fn erodata();
            fn sdata();
            fn edata();
        }
        let mid_text: VirtAddr = (stext as *const () as usize
            + (etext as *const () as usize - stext as *const () as usize) / 2)
            .into();
        let mid_rodata: VirtAddr = (srodata as *const () as usize
            + (erodata as *const () as usize - srodata as *const () as usize) / 2)
            .into();
        let mid_data: VirtAddr = (sdata as *const () as usize
            + (edata as *const () as usize - sdata as *const () as usize) / 2)
            .into();
        assert!(!self
            .find_valid_pte(mid_text.floor())
            .unwrap()
            .get_flags()
            .contains(RVPTEFlags::WRITEABLE));
        assert!(!self
            .find_valid_pte(mid_rodata.floor())
            .unwrap()
            .get_flags()
            .contains(RVPTEFlags::WRITEABLE));
        assert!(!self
            .find_valid_pte(mid_data.floor())
            .unwrap()
            .get_flags()
            .contains(RVPTEFlags::EXECUTABLE));
    }
    pub fn handle_mmap_read_page_fault(
        &mut self,
        vpn: VirtPageNum,
        ppn: PhysPageNum,
        vma_flags: MapPermission,
    ) {
        let mut pte_flags = RVPTEFlags::from(vma_flags);
        //可写的才需要cow
        if pte_flags.contains(RVPTEFlags::WRITEABLE) {
            pte_flags &= !RVPTEFlags::WRITEABLE;
            pte_flags |= RVPTEFlags::COW;
        }

        self.map_by_pte_flags(vpn, ppn, pte_flags);
    }
    pub fn handle_mmap_write_page_fault(&self, vpn: VirtPageNum, vma_flags: MapPermission) {
        let mut pte_flags = RVPTEFlags::from(vma_flags);

        //可写的才需要cow
        if pte_flags.contains(RVPTEFlags::WRITEABLE) {
            pte_flags &= !RVPTEFlags::WRITEABLE;
            pte_flags |= RVPTEFlags::COW;
        }
        // TODO: 可能低效
        if let Some(pte) = self.find_valid_pte(vpn) {
            let old_flag = pte.get_flags();
            pte.set_flags(pte_flags | old_flag);
        } else {
            panic!("found not(pfh)");
            self.map_by_pte_flags(vpn, 0.into(), pte_flags);
        }
    }
}
