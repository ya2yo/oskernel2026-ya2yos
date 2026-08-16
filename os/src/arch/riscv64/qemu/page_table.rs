use super::{
    memory_layout::{KERNEL_PGNUM_OFFSET, PAGE_SIZE, PAGE_SIZE_BITS},
    tlb::tlb_invalidate,
};
use crate::mm::{
    address::*, translate, FrameTracker, MapArea, MapPermission, MemorySetInner, KERNEL_SPACE,
};
use crate::syscall::MmapFlags;
use alloc::{sync::Arc, vec, vec::Vec};
use bitflags::*;
use riscv::register::satp;

use log::debug;

const MEGA_PAGE_NUM: usize = 1 << (21 - PAGE_SIZE_BITS);
const GIGA_PAGE_NUM: usize = 1 << (30 - PAGE_SIZE_BITS);

#[inline(always)]
pub fn get_token_from_regs() -> usize {
    satp::read().bits() & ((1 << 44) - 1)
}

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
        // A mapping becomes runnable immediately after page-fault return.  Do
        // not rely on a platform's optional hardware A-bit update mode: under
        // Svade, an A=0 leaf itself raises a page fault before the access can
        // proceed.  Linux likewise installs user PTEs with A already set.
        let mut flags = RVPTEFlags::VALID | RVPTEFlags::ACCESSED;
        if perm.contains(MapPermission::R) {
            flags.insert(RVPTEFlags::READABLE);
        }
        if perm.contains(MapPermission::W) {
            // RISC-V reserves the R=0, W=1 PTE encoding.  Keep the VMA's
            // logical permission unchanged, but normalize its hardware PTE
            // so a PROT_WRITE-only mapping is a valid writable leaf.
            flags.insert(RVPTEFlags::READABLE);
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
        for (i, idx) in idxs.iter().enumerate() {
            let pte = &mut ppn.as_array::<PageTableEntry>()[*idx];
            let flags = pte.get_flags();
            if i == 2
                || (i < 2
                    && flags.contains(RVPTEFlags::VALID)
                    && flags.intersects(
                        RVPTEFlags::READABLE | RVPTEFlags::WRITEABLE | RVPTEFlags::EXECUTABLE,
                    ))
            {
                return Some(pte);
            }
            if !flags.contains(RVPTEFlags::VALID) {
                return None;
            }
            ppn = pte.get_ppn();
        }
        None
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

    fn map_mega_page(&mut self, vpn: VirtPageNum, ppn: PhysPageNum, pte_flags: RVPTEFlags) {
        assert_eq!(vpn.0 % MEGA_PAGE_NUM, 0);
        assert_eq!(ppn.0 % MEGA_PAGE_NUM, 0);

        let indexes = vpn.indexes();
        let root_pte = &mut self.root_ppn.as_array::<PageTableEntry>()[indexes[0]];
        if !root_pte.get_flags().contains(RVPTEFlags::VALID) {
            let frame = FrameTracker::alloc().unwrap();
            *root_pte = PageTableEntry::new(frame.ppn, RVPTEFlags::VALID);
            self.frames.push(frame);
        }
        assert!(
            !root_pte
                .get_flags()
                .intersects(RVPTEFlags::READABLE | RVPTEFlags::WRITEABLE | RVPTEFlags::EXECUTABLE),
            "cannot place a 2MiB leaf below a 1GiB leaf"
        );

        let pte = &mut root_pte.get_ppn().as_array::<PageTableEntry>()[indexes[1]];
        if pte.get_flags().contains(RVPTEFlags::VALID)
            && !pte
                .get_flags()
                .intersects(RVPTEFlags::READABLE | RVPTEFlags::WRITEABLE | RVPTEFlags::EXECUTABLE)
        {
            // MAP_FIXED may leave an empty level-0 page table behind after
            // removing all of its 4 KiB leaves. Reclaim it before installing a
            // 2 MiB leaf; a live lower-level mapping still trips the assert.
            let child = pte.get_ppn().as_array::<PageTableEntry>();
            if child
                .iter()
                .all(|entry| !entry.get_flags().contains(RVPTEFlags::VALID))
            {
                pte.bits = 0;
            }
        }
        assert!(
            !pte.get_flags().contains(RVPTEFlags::VALID),
            "vpn {:?} is mapped before mapping",
            vpn
        );
        *pte = PageTableEntry::new(ppn, pte_flags | RVPTEFlags::VALID);
    }

    fn map_giga_page(&mut self, vpn: VirtPageNum, ppn: PhysPageNum, pte_flags: RVPTEFlags) {
        assert_eq!(vpn.0 % GIGA_PAGE_NUM, 0);
        assert_eq!(ppn.0 % GIGA_PAGE_NUM, 0);

        let pte = &mut self.root_ppn.as_array::<PageTableEntry>()[vpn.indexes()[0]];
        assert!(
            !pte.get_flags().contains(RVPTEFlags::VALID),
            "vpn {:?} is mapped before mapping",
            vpn
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
    /// RISC-V mappings do not flush the TLB per page; this mirrors `map` and
    /// exists so arch-independent bulk mappers share one code path.
    #[inline]
    pub fn map_no_flush(&mut self, vpn: VirtPageNum, ppn: PhysPageNum, flags: MapPermission) {
        self.map_by_pte_flags(vpn, ppn, RVPTEFlags::from(flags));
    }
    /// Map one user 2 MiB leaf without allocating 512 terminal PTEs.
    pub fn map_huge_page(&mut self, vpn: VirtPageNum, ppn: PhysPageNum, flags: MapPermission) {
        self.map_mega_page(vpn, ppn, RVPTEFlags::from(flags));
    }
    /// Invalidate the whole local TLB after a batch of `map_no_flush` calls.
    #[inline]
    pub fn flush_tlb_all(&self) {
        tlb_invalidate();
    }
    /// Direct-map an aligned kernel physical range with the largest Sv39 leaves
    /// possible. This is only used for the kernel's immutable direct map.
    pub fn map_direct_range(
        &mut self,
        start_vpn: VirtPageNum,
        end_vpn: VirtPageNum,
        flags: MapPermission,
    ) {
        let pte_flags = RVPTEFlags::from(flags);
        let mut vpn = start_vpn;
        while vpn < end_vpn {
            let remaining = end_vpn.0 - vpn.0;
            let ppn = PhysPageNum(vpn.0 - KERNEL_PGNUM_OFFSET);
            if vpn.0 % GIGA_PAGE_NUM == 0 && remaining >= GIGA_PAGE_NUM {
                self.map_giga_page(vpn, ppn, pte_flags);
                vpn.0 += GIGA_PAGE_NUM;
            } else if vpn.0 % MEGA_PAGE_NUM == 0 && remaining >= MEGA_PAGE_NUM {
                self.map_mega_page(vpn, ppn, pte_flags);
                vpn.0 += MEGA_PAGE_NUM;
            } else {
                self.map_by_pte_flags(vpn, ppn, pte_flags);
                vpn.0 += 1;
            }
        }
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
        let indexes = vpn.indexes();
        let mut table_ppn = self.root_ppn;
        for (level, index) in indexes.iter().enumerate() {
            let pte = &table_ppn.as_array::<PageTableEntry>()[*index];
            let flags = pte.get_flags();
            if !flags.contains(RVPTEFlags::VALID) {
                return None;
            }
            if flags
                .intersects(RVPTEFlags::READABLE | RVPTEFlags::WRITEABLE | RVPTEFlags::EXECUTABLE)
            {
                let lower_vpn_bits = (2 - level) * 9;
                let lower_vpn = vpn.0 & ((1 << lower_vpn_bits) - 1);
                return Some(PhysPageNum(pte.get_ppn().0 + lower_vpn));
            }
            table_ppn = pte.get_ppn();
        }
        None
    }
    /// Return the leaf PTE shape for an already mapped virtual page.
    ///
    /// This is diagnostic-only. In addition to the flags, retain the walk
    /// level and raw PTE so a fault report can detect an invalid, unaligned
    /// superpage leaf that a flag-only snapshot would conceal.
    #[cfg(feature = "fault-diagnostics")]
    pub fn translate_pte_diagnostic(&self, vpn: VirtPageNum) -> Option<(usize, usize, usize)> {
        let indexes = vpn.indexes();
        let mut table_ppn = self.root_ppn;
        for (level, index) in indexes.iter().enumerate() {
            let pte = &table_ppn.as_array::<PageTableEntry>()[*index];
            let flags = pte.get_flags();
            if !flags.contains(RVPTEFlags::VALID) {
                return None;
            }
            if flags
                .intersects(RVPTEFlags::READABLE | RVPTEFlags::WRITEABLE | RVPTEFlags::EXECUTABLE)
            {
                return Some((level, pte.bits, pte.get_ppn().0));
            }
            if level == 2 {
                return None;
            }
            table_ppn = pte.get_ppn();
        }
        None
    }
    /// Whether an existing leaf PTE still requires a COW write fault.
    ///
    /// Kernel-side copies use this to preserve the same COW boundary as a
    /// user-mode store instead of writing directly through a shared PPN.
    pub fn is_cow_page(&self, vpn: VirtPageNum) -> bool {
        self.find_valid_pte(vpn)
            .map(|pte| pte.get_flags().contains(RVPTEFlags::COW))
            .unwrap_or(false)
    }

    /// Whether the current software leaf permits an ordinary U-mode fetch.
    /// This is used only to distinguish a contradictory instruction-page
    /// fault from a genuine missing/protected mapping before one bounded
    /// retry.
    pub fn is_user_executable(&self, vpn: VirtPageNum) -> bool {
        self.find_valid_pte(vpn)
            .map(|pte| {
                let flags = pte.get_flags();
                flags.contains(RVPTEFlags::EXECUTABLE | RVPTEFlags::USER)
            })
            .unwrap_or(false)
    }

    /// Whether the current software leaf permits an ordinary U-mode load.
    ///
    /// A second thread can take a load fault for a page while the first
    /// thread is installing that same page under the address-space lock. Once
    /// the lock is reacquired, the PTE is present and the fault can be
    /// retried after flushing the stale translation.
    pub fn is_user_readable(&self, vpn: VirtPageNum) -> bool {
        self.find_valid_pte(vpn)
            .map(|pte| {
                let flags = pte.get_flags();
                flags.contains(RVPTEFlags::READABLE | RVPTEFlags::USER)
            })
            .unwrap_or(false)
    }

    /// Whether the current software leaf permits an ordinary U-mode store.
    ///
    /// A COW leaf deliberately remains ineligible even if a stale writable
    /// bit were observed: callers must take the normal write-fault path so
    /// the private frame is installed before directly accessing user VA.
    pub fn is_user_writable(&self, vpn: VirtPageNum) -> bool {
        self.find_valid_pte(vpn)
            .map(|pte| {
                let flags = pte.get_flags();
                flags.contains(RVPTEFlags::WRITEABLE | RVPTEFlags::USER)
                    && !flags.contains(RVPTEFlags::COW)
            })
            .unwrap_or(false)
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
    /// 修改指定虚拟页的硬件页表项权限位（mprotect 的页表层）。
    ///
    /// 内部调用 `find_pte_create`（如 PTE 不存在则自动创建后修改），
    /// 将 `add_flags` 指定的权限位"添加"到已有标志上（位或操作）。
    ///
    /// # 注意
    /// - `find_pte_create` 会为懒分配尚未映射的页创建新的页表项，这可能导致
    ///   未实际分配物理页的 VPN 也获得页表项。loongarch 架构的实现不同：
    ///   使用 `find_valid_pte` 仅对已映射的页表项修改权限。
    /// - 新旧标志做位或(`|`)而非替换，因此当前实现不会移除已有权限。
    ///   这可能导致"只降不升"的语义偏差（真正的 mprotect 应完全替换权限）。
    pub fn handle_mprotect(&mut self, vpn: VirtPageNum, add_flags: MapPermission) {
        let Some(pte) = self.find_valid_pte(vpn) else {
            return;
        };
        let old_flags = pte.get_flags();
        // Do not use `RVPTEFlags::from` here: mprotect also visits lazy PTEs
        // and must not make their PPN=0 entries valid.  It still needs the
        // architectural W => R normalization for present entries.
        let mut requested_flags = RVPTEFlags::from_bits_truncate(add_flags.bits() as usize);
        if requested_flags.contains(RVPTEFlags::WRITEABLE) {
            requested_flags.insert(RVPTEFlags::READABLE);
        }
        pte.set_flags(requested_flags | old_flags);
    }
    /// return: 若成功处理了 present PTE 的写保护页错误，返回true，否则返回false
    pub fn handle_write_protect_page_fault(&mut self, va: VirtAddr, vma: &mut MapArea) -> bool {
        debug!("[handle_write_protect_page_fault] va={:?}", va);
        let vpn = va.floor();
        let pte = match self.find_valid_pte(vpn) {
            Some(pte) => pte,
            None => return false,
        };

        let mut flags = pte.get_flags();
        if !flags.contains(RVPTEFlags::COW) {
            if flags.contains(RVPTEFlags::WRITEABLE) {
                // A writable PTE can fault only because the hardware dirty
                // state needs refreshing; it is not a COW split.
                flags.insert(RVPTEFlags::DIRTY);
                pte.set_flags(flags);
                tlb_invalidate();
                return true;
            }
            // Do not turn ELF text or an intentionally read-only mapping
            // into a writable COW page.
            return false;
        }

        // 必须有对应的 frame（ELF 段用 data_frames 跟踪）
        // fork 子进程的 Brk 区域可能存在无 data_frames 条目的 COW PTE；
        // 此时仍应分配新帧并复制数据。
        let (refcnt, source_frame) = match vma.data_frames.get(&vpn) {
            Some(frame) => {
                // Count before pinning: the temporary pin must not turn an
                // exclusively mapped page into a shared one.
                let refcnt = Arc::strong_count(frame);
                (refcnt, Some(Arc::clone(frame)))
            }
            None => (2, None), // no tracker → force copy path
        };
        debug!("---> refcnt={}", refcnt);
        // 只有一个引用：无需复制物理页，直接调整权限即可
        if refcnt == 1 {
            let mut flags = pte.get_flags();
            flags.remove(RVPTEFlags::COW); // 无 COW 时是空操作
            flags.insert(RVPTEFlags::WRITEABLE);
            flags.insert(RVPTEFlags::READABLE);
            flags.insert(RVPTEFlags::DIRTY);
            pte.set_flags(flags);
            tlb_invalidate();
            return true;
        }

        // Linux's wp_page_copy() keeps the old folio referenced while it
        // allocates and copies the replacement page, then swaps the PTE.
        // Keep the same order here: do not tear down a working mapping before
        // allocation succeeds, and keep source_frame pinned through the copy.
        // `translate` includes the VPN offset for a superpage leaf.  The raw
        // PTE contains only the aligned base PPN.
        let src_ppn = self.translate(vpn).unwrap_or_else(|| pte.get_ppn());
        if let Some(frame) = source_frame.as_ref() {
            if frame.ppn != src_ppn {
                return false;
            }
        }
        let new_frame = match FrameTracker::alloc() {
            Some(frame) => frame,
            None => return false, // Keep the old PTE and mapping intact on OOM.
        };
        let new_ppn = new_frame.ppn;
        new_ppn
            .bytes_array_mut()
            .copy_from_slice(src_ppn.bytes_array());

        flags = pte.get_flags();
        flags.remove(RVPTEFlags::COW);
        flags.insert(RVPTEFlags::WRITEABLE);
        flags.insert(RVPTEFlags::DIRTY);
        *pte = PageTableEntry::new(new_ppn, flags);
        tlb_invalidate();

        // The current VMA no longer maps the old frame.  Drop its old Arc
        // only after the PTE has switched and the local TLB is invalidated.
        let old_frame = vma.data_frames.insert(vpn, new_frame);
        drop(old_frame);
        drop(source_frame);

        true
    }
    /// 处理页表项的COW映射：将可写的页转换为COW页
    pub fn handle_cow_mapping_from_exited_user(
        &self,
        vpn: VirtPageNum,
        memory_set: &mut MemorySetInner,
    ) {
        let src_ppn = match self.translate(vpn) {
            Some(ppn) => ppn,
            None => return,
        };
        let Some(pte) = self.find_valid_pte(vpn) else {
            // ELF/Brk VMAs may cover lazy or already-unmapped pages.  Fork
            // only needs to COW a present leaf; a missing leaf is inherited
            // lazily by the child and must not turn fork into a kernel panic.
            return;
        };
        let mut pte_flags = pte.get_flags();
        // 对于可写的页，或者有写时复制的标志位的页
        // 需要考虑写时复制
        if pte_flags.contains(RVPTEFlags::WRITEABLE) {
            // COW 共享页不能继续保留 writable/dirty，否则 fork 后仍可能写共享页。
            pte_flags &= !RVPTEFlags::WRITEABLE;
            pte_flags &= !RVPTEFlags::DIRTY;
            pte_flags |= RVPTEFlags::COW;
        }
        pte.set_flags(pte_flags);
        tlb_invalidate();
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
        mmap_flags: MmapFlags,
    ) {
        let mut pte_flags = RVPTEFlags::from(vma_flags);
        if mmap_flags.contains(MmapFlags::MAP_SHARED) {
            if pte_flags.contains(RVPTEFlags::WRITEABLE) {
                pte_flags.insert(RVPTEFlags::DIRTY);
            }
        } else if pte_flags.contains(RVPTEFlags::WRITEABLE) {
            pte_flags &= !RVPTEFlags::WRITEABLE;
            pte_flags &= !RVPTEFlags::DIRTY;
            pte_flags |= RVPTEFlags::COW;
        }

        self.map_by_pte_flags(vpn, ppn, pte_flags);
        tlb_invalidate();
    }
    pub fn handle_mmap_write_page_fault(
        &self,
        vpn: VirtPageNum,
        vma_flags: MapPermission,
        mmap_flags: MmapFlags,
    ) {
        let mut pte_flags = RVPTEFlags::from(vma_flags);

        if mmap_flags.contains(MmapFlags::MAP_SHARED) {
            if pte_flags.contains(RVPTEFlags::WRITEABLE) {
                pte_flags.insert(RVPTEFlags::DIRTY);
            }
        } else if pte_flags.contains(RVPTEFlags::WRITEABLE) {
            pte_flags &= !RVPTEFlags::WRITEABLE;
            pte_flags &= !RVPTEFlags::DIRTY;
            pte_flags |= RVPTEFlags::COW;
        }
        // TODO: 可能低效
        if let Some(pte) = self.find_valid_pte(vpn) {
            let old_flag = pte.get_flags();
            pte.set_flags(pte_flags | old_flag);
            tlb_invalidate();
        } else {
            panic!("found not(pfh)");
            self.map_by_pte_flags(vpn, 0.into(), pte_flags);
        }
    }
}
