use core::arch::asm;

use super::{
    memory_layout::{KERNEL_PGNUM_OFFSET, PAGE_SIZE, PAGE_SIZE_BITS},
    tlb::tlb_invalidate,
};
use crate::{
    arch::memory_layout::{self, KERNEL_ADDR_OFFSET},
    mm::{
        address::*, cma_alloc, FrameTracker, MapArea, MapPermission, MemorySetInner, KERNEL_SPACE,
    },
};
use alloc::{sync::Arc, vec, vec::Vec};
use bitflags::*;
use log::{debug, trace, warn};
use loongArch64::register::{asid, crmd, pgdh, pgdl, tlbrbadv, tlbrelo0, tlbrelo1};
use xmas_elf::program::Flags;

#[inline(always)]
pub fn get_token_from_regs() -> usize {
    let low = pgdl::read().base();
    let high = pgdh::read().base();
    assert!(low == high);
    assert!(low & 0xFFF == 0);
    low >> 12
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
    /// Page Table Entry flags
    pub struct LAPTEFlags: usize {
        /// Valid Bit
        const VALID = 1 << 0;
        /// Dirty Bit, true if it is modified.
        const DIRTY = 1 << 1;
        /// Privilege Level field
        const PLV0 = 0;
        const PLV1 = 1 << 2;
        const PLV2 = 2 << 2;
        const PLV3 = 3 << 2;
        /// Memory Access Type: Strongly-ordered UnCached (SUC)
        const MAT_SUC = 0 << 4;
        /// Memory Access Type: Coherent Cached (CC)
        const MAT_CC = 1 << 4;
        /// Memory Access Type: Weakly-ordered UnCached (WUC)
        const MAT_WUC = 2 << 4;
        /// Global Bit (Basic PTE)
        const GLOBAL = 1 << 6;
        /// Physical Bit, whether the physical page exists
        const P = 1 << 7;
        /// Writable Bit
        const WRITEABLE = 1 << 8;
        /// Copy-On-Write Bit
        const COW = 1 << 9;

        /// Not Readable Bit
        const UNREADEABLE = 1 << (usize::BITS-3); // 61
        /// Executable Bit
        const UNEXECUTABLE = 1 << (usize::BITS-2); // 62
        /// Restricted Privilege LeVel enable (RPLV) for the page table.
        /// When RPLV=0, the page table entry can be accessed by any program whose privilege level is not lower than PLV;
        /// when RPLV=1, the page table entry can only be accessed by programs whose privilege level is equal to PLV.
        const RPLV = 1 << (usize::BITS-1); // 63
    }
}

impl From<MapPermission> for LAPTEFlags {
    fn from(perm: MapPermission) -> Self {
        // TODO:是否可以通过编译检查，将这里的if判断去掉，因为MapPermission是和LAPTEFlags正好一一对应
        let mut flags = LAPTEFlags::VALID | LAPTEFlags::MAT_CC | LAPTEFlags::P;
        if !perm.contains(MapPermission::R) {
            flags.insert(LAPTEFlags::UNREADEABLE);
        }
        if perm.contains(MapPermission::W) {
            flags.insert(LAPTEFlags::WRITEABLE);
        }
        if !perm.contains(MapPermission::X) {
            flags.insert(LAPTEFlags::UNEXECUTABLE);
        }
        if perm.contains(MapPermission::U) {
            flags.insert(LAPTEFlags::PLV3);
        }
        flags
    }
}

impl From<LAPTEFlags> for MapPermission {
    fn from(flags: LAPTEFlags) -> Self {
        let mut perm = MapPermission::empty();
        if !flags.contains(LAPTEFlags::UNREADEABLE) {
            perm.insert(MapPermission::R);
        }
        if flags.contains(LAPTEFlags::WRITEABLE) {
            perm.insert(MapPermission::W);
        }
        if !flags.contains(LAPTEFlags::UNEXECUTABLE) {
            perm.insert(MapPermission::X);
        }
        if flags.contains(LAPTEFlags::PLV3) {
            perm.insert(MapPermission::U);
        }
        perm
    }
}

// impl From<LAPTEFlags> for PTEFlags {
//     fn from(flags: LAPTEFlags) -> Self {
//         let mut result = PTEFlags::empty();
//         if flags.contains(LAPTEFlags::VALID) {
//             result.insert(PTEFlags::VALID);
//         }
//         if !flags.contains(LAPTEFlags::UNREADEABLE) {
//             result.insert(PTEFlags::READABLE);
//         }
//         if flags.contains(LAPTEFlags::WRITEABLE) {
//             result.insert(PTEFlags::WRITEABLE);
//         }
//         if !flags.contains(LAPTEFlags::UNEXECUTABLE) {
//             result.insert(PTEFlags::EXECUTABLE);
//         }
//         if flags.contains(LAPTEFlags::PLV3) {
//             result.insert(PTEFlags::USER);
//         }
//         if flags.contains(LAPTEFlags::COW) {
//             result.insert(PTEFlags::COW);
//         }
//         result
//     }
// }

// impl From<PTEFlags> for LAPTEFlags {
//     fn from(flags: PTEFlags) -> Self {
//         let mut result = LAPTEFlags::MAT_CC | LAPTEFlags::P;
//         if flags.contains(PTEFlags::VALID) {
//             result.insert(LAPTEFlags::VALID);
//         }
//         if !flags.contains(PTEFlags::READABLE) {
//             result.insert(LAPTEFlags::UNREADEABLE);
//         }
//         if flags.contains(PTEFlags::WRITEABLE) {
//             result.insert(LAPTEFlags::WRITEABLE);
//         }
//         if !flags.contains(PTEFlags::EXECUTABLE) {
//             result.insert(LAPTEFlags::UNEXECUTABLE);
//         }
//         if flags.contains(PTEFlags::USER) {
//             result.insert(LAPTEFlags::PLV3);
//         }
//         if flags.contains(PTEFlags::COW) {
//             result.insert(LAPTEFlags::COW);
//         }
//         result
//     }
// }

/// Page Table Entry
#[derive(Copy, Clone)]
#[repr(C)]
struct PageTableEntry {
    bits: usize,
}
impl PageTableEntry {
    // TODO:这里48为palen，需要完善一下对palen是否等于48的检查
    const PPN_MASK: usize = ((1usize << 49) - 1) << PAGE_SIZE_BITS;
    #[inline(always)]
    fn new(ppn: PhysPageNum, flags: LAPTEFlags) -> Self {
        PageTableEntry {
            bits: ((ppn.0 << PAGE_SIZE_BITS) & Self::PPN_MASK) | flags.bits as usize,
        }
    }
    #[inline(always)]
    fn get_ppn(&self) -> PhysPageNum {
        ((self.bits & Self::PPN_MASK) >> PAGE_SIZE_BITS).into()
    }
    #[inline(always)]
    fn get_flags(&self) -> LAPTEFlags {
        LAPTEFlags {
            bits: self.bits & !Self::PPN_MASK,
        }
    }
    #[inline(always)]
    fn set_flags(&mut self, flags: LAPTEFlags) {
        self.bits = (flags.bits as usize) | (self.bits & Self::PPN_MASK);
    }
    // pub fn set_uncached(&mut self) {
    //     let mut flags = self.get_flags();
    //     flags.remove(LAPTEFlags::MAT_CC);
    //     flags.remove(LAPTEFlags::MAT_WUC);
    //     flags.insert(LAPTEFlags::MAT_SUC);
    //     self.set_flags(flags);
    // }
}

pub struct PageTable {
    root_ppn: PhysPageNum,
    frames: Vec<Arc<FrameTracker>>,
}
// 实现页表的私有方法
impl PageTable {
    /// Find the page in the page table, creating the page on the way if not exists.
    /// Note: It does NOT create the terminal node. The caller must verify its validity and create according to his own needs.
    fn find_pte_create(&mut self, vpn: VirtPageNum) -> Option<&mut PageTableEntry> {
        let idxs = vpn.indexes();
        let mut ppn = self.root_ppn;
        let mut result: Option<&mut PageTableEntry> = None;

        for (i, idx) in idxs.iter().enumerate() {
            if i == 2 {
                let pte = &mut ppn.as_array::<PageTableEntry>()[*idx];
                result = Some(pte);
                break;
            } else {
                let next = &mut ppn.as_array::<usize>()[*idx];
                if *next == 0 {
                    // 目录项无效
                    let frame = FrameTracker::alloc().unwrap();
                    *next = PhysAddr::from(frame.ppn).0;
                    self.frames.push(frame);
                }
                ppn = PhysAddr::from(*next).floor();
            }
        }
        result
    }
    /// 如果找不到则会返回None，如果找到了但是not valid仍然会返回
    fn find_pte(&self, vpn: VirtPageNum) -> Option<&mut PageTableEntry> {
        let idxs = vpn.indexes();
        let mut ppn = self.root_ppn;
        for (i, idx) in idxs.iter().enumerate() {
            if ppn.0 == 0 {
                return None;
            }
            if i == 2 {
                let pte = &mut ppn.as_array::<PageTableEntry>()[*idx];
                return Some(pte);
            } else {
                let next = ppn.as_array::<usize>()[*idx];
                ppn = PhysAddr::from(next).floor();
            }
        }
        return None;
    }
    /// return: 有效的页表项
    fn find_valid_pte(&self, vpn: VirtPageNum) -> Option<&mut PageTableEntry> {
        self.find_pte(vpn)
            .filter(|pte| pte.get_flags().contains(LAPTEFlags::VALID))
            .map(|pte| pte)
    }
    fn map_by_pte_flags(&mut self, vpn: VirtPageNum, ppn: PhysPageNum, pte_flags: LAPTEFlags) {
        let pte = self.find_pte_create(vpn).unwrap();
        assert!(
            !pte.get_flags().contains(LAPTEFlags::VALID),
            "vpn {:?}, va {:x} is mapped before mapping",
            vpn,
            vpn.0 << PAGE_SIZE_BITS
        );
        *pte = PageTableEntry::new(ppn, pte_flags | LAPTEFlags::VALID);
        tlb_invalidate(); // 保险起见 TODO:不刷新是不是也可以？
    }
}
// 实现PageTable的通用函数
impl PageTable {
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
        let mut ret = PageTable {
            root_ppn: frame.ppn,
            frames: vec![frame],
        };
        // HXC：我想loongarch的用户页表需要这个
        ret.map(
            VirtAddr::from(memory_layout::sigreturn_va()).floor(),
            PhysAddr::from(memory_layout::sigreturn_pa()).floor(),
            MapPermission::X | MapPermission::R | MapPermission::U,
        );
        ret
    }
    pub fn activate(&self) {
        asid::set_asid_width(0);
        // 用户和内核共用一个页表
        let pa = PhysAddr::from(self.root_ppn).0;
        pgdl::set_base(pa);
        pgdh::set_base(pa);
        tlb_invalidate();
    }
    pub fn clear(&mut self) {
        self.frames.clear();
    }
    /// Find the page in the page table, creating the page on the way if not exists.
    /// Note: It does NOT create the terminal node. The caller must verify its validity and create according to his own needs.
    /// Map the `vpn` to `ppn` with the `flags`.
    /// # Note
    /// Allocation should be done elsewhere.
    /// # Exceptions
    /// Panics if the `vpn` is mapped.
    #[inline]
    pub fn map(&mut self, vpn: VirtPageNum, ppn: PhysPageNum, flags: MapPermission) {
        self.map_by_pte_flags(vpn, ppn, LAPTEFlags::from(flags));
    }
    /// Unmap the `vpn` to `ppn` with the `flags`.
    /// # Exceptions
    /// Panics if the `vpn` is NOT mapped (invalid).
    pub fn unmap(&mut self, vpn: VirtPageNum) {
        // 如果不存在,即lazy allocation,跳过即可
        if let Some(pte) = self.find_pte(vpn) {
            if pte.get_flags().contains(LAPTEFlags::VALID) {
                *pte = PageTableEntry { bits: 0 };
            }
        }
    }
    /// Translate the `vpn` into its corresponding `Some(PageTableEntry)` if exists
    /// `None` is returned if nothing is found.
    pub fn translate(&self, vpn: VirtPageNum) -> Option<PhysPageNum> {
        self.find_pte(vpn)
            .filter(|pte| pte.get_flags().contains(LAPTEFlags::VALID))
            .map(|pte| pte.get_ppn())
    }
    /// Translate the virtual address into its corresponding `PhysAddr` if mapped in current page table.
    /// `None` is returned if nothing is found.
    pub fn translate_va(&self, va: VirtAddr) -> Option<PhysAddr> {
        // self.find_pte(va.floor())
        //     .filter(|pte| pte.get_flags().contains(LAPTEFlags::VALID))
        //     .map(|pte| {
        //         let aligned_pa: PhysAddr = pte.get_ppn().into();
        //         let offset = va.page_offset();
        //         let aligned_pa_usize: usize = aligned_pa.into();
        //         (aligned_pa_usize + offset).into()
        //     })
        let vpn = va.floor();
        let ppn = self.translate(vpn)?;
        let offset = va.page_offset();
        let aligned_pa: PhysAddr = PhysAddr::from(ppn);
        let aligned_pa_usize: usize = aligned_pa.into();
        Some((aligned_pa_usize + offset).into())
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
        pte.set_flags(LAPTEFlags::from_bits_truncate(add_flags.bits() as usize) | old_flags);
    }
    /// return: 若错误是COW且成功处理了COW页错误，返回true，否则返回false
    pub fn handle_cow_page_fault(&mut self, va: VirtAddr, vma: &mut MapArea) -> bool {
        let pte = match self.find_valid_pte(va.floor()) {
            Some(pte) => pte,
            None => return false,
        };
        let pte_flags = pte.get_flags();
        if !pte_flags.contains(LAPTEFlags::COW) {
            // 这不是COW写错误，交给上层按普通用户页错误处理。
            return false;
        }

        // 只有一个，说明父进程已经释放，不用复制
        let frame = vma.data_frames.get(&va.into()).unwrap();

        if Arc::strong_count(frame) == 1 {
            let mut flags = pte.get_flags();
            flags.remove(LAPTEFlags::COW);
            flags.insert(LAPTEFlags::WRITEABLE);
            flags.insert(LAPTEFlags::DIRTY);
            pte.set_flags(flags);
            tlb_invalidate();
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
        flags.remove(LAPTEFlags::COW);
        flags.insert(LAPTEFlags::WRITEABLE);
        flags.insert(LAPTEFlags::DIRTY);
        pte.set_flags(flags);
        tlb_invalidate();

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
        if pte_flags.contains(LAPTEFlags::WRITEABLE) {
            pte_flags &= !LAPTEFlags::WRITEABLE;
            pte_flags &= !LAPTEFlags::DIRTY;
            pte_flags |= LAPTEFlags::COW;
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
            .contains(LAPTEFlags::WRITEABLE));
        assert!(!self
            .find_valid_pte(mid_rodata.floor())
            .unwrap()
            .get_flags()
            .contains(LAPTEFlags::WRITEABLE));
        assert!(!self
            .find_valid_pte(mid_data.floor())
            .unwrap()
            .get_flags()
            .contains(!LAPTEFlags::UNEXECUTABLE));
    }
    pub fn handle_mmap_read_page_fault(
        &mut self,
        vpn: VirtPageNum,
        ppn: PhysPageNum,
        vma_flags: MapPermission,
    ) {
        let mut pte_flags = LAPTEFlags::from(vma_flags);
        //可写的才需要cow
        if pte_flags.contains(LAPTEFlags::WRITEABLE) {
            pte_flags &= !LAPTEFlags::WRITEABLE;
            pte_flags &= !LAPTEFlags::DIRTY;
            pte_flags |= LAPTEFlags::COW;
        }

        self.map_by_pte_flags(vpn, ppn, pte_flags);
    }
    pub fn handle_mmap_write_page_fault(&self, vpn: VirtPageNum, vma_flags: MapPermission) {
        let mut pte_flags = LAPTEFlags::from(vma_flags);

        //可写的才需要cow
        if pte_flags.contains(LAPTEFlags::WRITEABLE) {
            pte_flags &= !LAPTEFlags::WRITEABLE;
            pte_flags &= !LAPTEFlags::DIRTY;
            pte_flags |= LAPTEFlags::COW;
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
    /// 该函数用于测试页表机制实现是否正确
    /// 其机理是，分配一个物理页，将其映射到内核页表上
    /// 并试图访问这个物理页上的东西
    /// 调用此函数时，可以确保self(内核页表)已经被启用
    pub fn kernel_pagetable_test_func(&mut self) {
        debug!("pg:{}", crmd::read().pg());
        debug!("da:{}", crmd::read().da());
        let va = 0x20000 as usize;
        let ptr = va as *mut u64;
        let va = VirtAddr::from(va);
        let vpn = va.floor();
        let pa = cma_alloc(1).unwrap();
        let ppn = pa.floor();
        println!("ppn={:#x}", ppn.0);
        let flags = LAPTEFlags::VALID | LAPTEFlags::MAT_SUC | LAPTEFlags::P;

        let pte = self.find_pte_create(vpn).unwrap();
        *pte = PageTableEntry::new(ppn, flags);
        tlb_invalidate();
        println!("try to translate");
        let x = self.translate_va(va).unwrap().0;
        println!("translated: x={:#x}", x); // 正常工作

        walk_page_table(pgdl::read().base(), 0x20000);
        page_wake_lddir(0x20000);
        println!("test ready");
        unsafe {
            let x = ptr.read_volatile();
            println!("test succ: x={}", x);
        }
    }
    pub fn set_dirty_bit(&mut self, vpn: VirtPageNum) -> Result<(), ()> {
        tlb_invalidate();
        if let Some(pte) = self.find_pte(vpn) {
            pte.set_flags(pte.get_flags() | LAPTEFlags::DIRTY);
            Ok(())
        } else {
            Err(())
        }
    }
}

fn walk_page_table(pgd: usize, va: usize) {
    let vpn2 = (va >> 30) & 0x1FF;
    let vpn1 = (va >> 21) & 0x1FF;
    let vpn0 = (va >> 12) & 0x1FF;
    println!("va={:#x}, vpn=[{},{},{}]", va, vpn2, vpn1, vpn0);
    let mut p: *const usize;

    let pte2 = unsafe {
        p = (pgd + vpn2 * 8) as *const usize;
        println!("p={:#x}", p as usize);
        p = p.byte_add(KERNEL_ADDR_OFFSET);
        *p
    };
    println!("PGD[{:#x}] = {:#x}", vpn2, pte2);
    let pte1 = unsafe {
        p = ((pte2 & !0xFFF) + vpn1 * 8) as *const usize;
        println!("p={:#x}", p as usize);
        p = p.byte_add(KERNEL_ADDR_OFFSET);
        *p
    };
    println!("PMD[{:#x}] = {:#x}", vpn1, pte1);
    let pte0 = unsafe {
        p = ((pte1 & !0xFFF) + vpn0 * 8) as *const usize;
        println!("p={:#x}", p as usize);
        p = p.byte_add(KERNEL_ADDR_OFFSET);
        *p
    };
    println!("PTE[{:#x}] = {:#x}", vpn0, pte0);
}

fn lddir2(rj: usize) -> usize {
    let rd: usize;
    unsafe {
        asm!("lddir {}, {}, 2", out(reg) rd, in(reg) rj);
    }
    return rd;
}

fn lddir1(rj: usize) -> usize {
    let rd: usize;
    unsafe {
        asm!("lddir {}, {}, 1", out(reg) rd, in(reg) rj);
    }
    return rd;
}

fn ldpte(rj: usize) -> usize {
    unsafe {
        asm!("ldpte {}, 0", in(reg) rj);
    }
    let l0 = tlbrelo0::read().raw();
    let l1 = tlbrelo1::read().raw();
    println!("relo0={:#x}", l0);
    println!("relo1={:#x}", l1);

    return l0 + l1;
}

fn page_wake_lddir(va: usize) {
    let pgd = pgdl::read().base();
    println!("pgd={:#x}, va={:#x}", pgd, va);
    tlbrbadv::set_vaddr(va);
    let mut p;
    p = lddir2(pgd);
    println!("p1={:#x}", p);
    p &= !0xFFF;
    p = lddir1(p);
    println!("p2={:#x}", p);
    p &= !0xFFF;
    p = ldpte(p);
    println!("p3={:#x}", p);
}
