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
                    // 将修改的内容写回文件
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
                                        // 说明可以拼接起来
                                        wb_range.push((end_range.0, VirtPageNum(vpn.0 + 1)));
                                    } else {
                                        // 分开的页面
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

    /// 修改一段虚拟地址空间的访问权限（mprotect 核心逻辑）。
    ///
    /// 此函数完成两件事：
    /// 1. **逻辑段（MapArea）拆分**：将现有 area 按 `[start_vpn, end_vpn)` 范围切分，
    ///    确保范围内的区域获得新权限，范围外的区域保持原权限不变。
    /// 2. **页表项更新**：遍历范围内的每个 VPN，调用 `handle_mprotect` 修改硬件页表项的权限位。
    ///
    /// # 参数
    /// - `start_vpn`, `end_vpn`: 目标虚拟页号范围（左闭右开）。
    /// - `map_perm`: 要设置的新权限。
    /// - `file` / `offset` / `if_mmap`: mmap 调用时传入的文件和偏移信息；
    ///   普通 mprotect 系统调用时 `if_mmap=false`，这些参数不使用。
    ///
    /// # Area 拆分策略（四种情况）
    ///
    /// 对每个现有 area，根据其与目标范围 `[start_vpn, end_vpn)` 的位置关系处理：
    ///
    /// **情况1**：area 完全在范围内 (`start >= start_vpn && end <= end_vpn`)
    /// → 直接修改整个 area 的权限即可，无需拆分。
    ///
    /// **情况2**：area 左侧伸出范围 (`start < start_vpn && end` 在范围内)
    /// → 拆分为两个 area：
    ///   - 左半部 `[start, start_vpn)`: 保持原权限
    ///   - 右半部 `[start_vpn, end)`: 设为新权限
    /// → data_frames 按页号重新分配（VPN >= start_vpn 的帧移到右半部）。
    ///
    /// **情况3**：area 右侧伸出范围 (`start` 在范围内 `&& end > end_vpn`)
    /// → 拆分为两个 area：
    ///   - 左半部 `[start, end_vpn)`: 设为新权限
    ///   - 右半部 `[end_vpn, end)`: 保持原权限
    /// → data_frames 按页号重新分配（VPN < end_vpn 的帧移到左半部）。
    ///
    /// **情况4**：area 完全包围范围 (`start < start_vpn && end > end_vpn`)
    /// → 拆分为三个 area：
    ///   - 前部 `[start, start_vpn)`: 保持原权限
    ///   - 中部 `[start_vpn, end_vpn)`: 设为新权限（在原 area 上就地修改）
    ///   - 后部 `[end_vpn, end)`: 保持原权限
    /// → data_frames 按页号三段式重新分配。
    ///
    /// # 注意
    /// - 跨 area 边界的 mprotect 通过多次拆分自然处理：每个 area 独立判断，
    ///   不重叠的 area 会被忽略（跳过循环体）。
    /// - 新创建的 area 先存入 `new_areas`，在遍历完所有 area 后统一插入，
    ///   避免在迭代 `self.areas` 的同时修改 Vec 导致迭代器失效。
    /// - 最后必须 `tlb_invalidate()` 以刷新 TLB，确保新权限立即生效。
    pub fn mprotect(
        &mut self,
        start_vpn: VirtPageNum,
        end_vpn: VirtPageNum,
        map_perm: MapPermission,
        file: Option<Arc<OSFile>>,
        offset: usize,
        if_mmap: bool,
    ) {
        // 防御性检查：如果范围无效（start >= end）则直接返回
        if start_vpn >= end_vpn {
            warn!(
                "[mprotect] invalid range: start_vpn={:?} >= end_vpn={:?}",
                start_vpn, end_vpn
            );
            return;
        }
        // 收集拆分过程中新产生的 area，遍历结束后再统一插入
        let mut new_areas = Vec::new();
        for area in self.areas.iter_mut() {
            let (start, end) = area.vpn_range.range();
            // 情况1：area 完全被目标范围覆盖
            // 例: area=[4,8) 被 [3,10) 覆盖 → 整个 area 改权限
            if start >= start_vpn && end <= end_vpn {
                area.map_perm = map_perm;
                if if_mmap {
                    area.mmap_file.file = file.clone();
                }
                if offset != usize::MAX {
                    area.mmap_file.offset = offset as usize;
                }
                continue;
            // 情况2：area 左侧在范围外，右侧在范围内
            // 例: area=[2,6) 目标=[4,9) → 拆为 [2,4)原权限 + [4,6)新权限
            } else if start < start_vpn && end > start_vpn && end <= end_vpn {
                // new_area: 右半部，获得新权限
                let mut new_area = MapArea::from_another(area);
                new_area.map_perm = map_perm;
                new_area.vpn_range = VPNRange::new(start_vpn, end);
                if if_mmap {
                    new_area.mmap_file.file = file.clone();
                }
                if offset != usize::MAX {
                    new_area.mmap_file.offset = offset as usize;
                }
                // area: 左半部，保持原权限，收缩范围
                area.vpn_range = VPNRange::new(start, start_vpn);
                // 注册到共享内存组
                GROUP_SHARE.lock().add_area(new_area.groupid);
                // 将 VPN >= start_vpn 的已分配物理帧从 area 迁移到 new_area
                // data_frames 为 BTreeMap，pop_last 从大 VPN 开始取，
                // 遇到 start_vpn 时说明已到达分界点，停止迁移
                while !area.data_frames.is_empty() {
                    let page = area.data_frames.pop_last().unwrap();
                    new_area.data_frames.insert(page.0, page.1);
                    if page.0 == start_vpn {
                        break;
                    }
                }
                new_areas.push(new_area);
                continue;
            // 情况3：area 左侧在范围内，右侧在范围外
            // 例: area=[5,10) 目标=[3,8) → 拆为 [5,8)新权限 + [8,10)原权限
            } else if start >= start_vpn && start < end_vpn && end > end_vpn {
                // new_area: 左半部，获得新权限
                let mut new_area = MapArea::from_another(area);
                new_area.map_perm = map_perm;
                new_area.vpn_range = VPNRange::new(start, end_vpn);
                if if_mmap {
                    new_area.mmap_file.file = file.clone();
                }
                if offset != usize::MAX {
                    new_area.mmap_file.offset = offset as usize;
                }
                // area: 右半部，保持原权限，收缩范围
                area.vpn_range = VPNRange::new(end_vpn, end);
                // 注册到共享内存组
                GROUP_SHARE.lock().add_area(new_area.groupid);
                // 将 VPN < end_vpn 的已分配物理帧从 area 迁移到 new_area
                // data_frames 为 BTreeMap，pop_first 从小 VPN 开始取，
                // 遇到 VPN >= end_vpn 时说明剩余帧都属于右半部，停止迁移
                while !area.data_frames.is_empty() {
                    let page = area.data_frames.pop_first().unwrap();
                    if page.0 >= end_vpn {
                        // 此帧属于右半部，放回原 area
                        area.data_frames.insert(page.0, page.1);
                        break;
                    }
                    new_area.data_frames.insert(page.0, page.1);
                }
                new_areas.push(new_area);
                continue;
            // 情况4：area 完全包围目标范围
            // 例: area=[2,12) 目标=[4,8) → 拆为三个：
            //     [2,4)原权限 + [4,8)新权限(原地修改) + [8,12)原权限
            } else if start < start_vpn && end > end_vpn {
                let mut front_area = MapArea::from_another(area);
                let mut back_area = MapArea::from_another(area);
                // area：中部，赋新权限（就地修改）
                area.map_perm = map_perm;
                front_area.vpn_range = VPNRange::new(start, start_vpn); // 前部，原权限
                back_area.vpn_range = VPNRange::new(end_vpn, end); // 后部，原权限
                area.vpn_range = VPNRange::new(start_vpn, end_vpn); // 中部，新权限
                if if_mmap {
                    area.mmap_file.file = file.clone();
                }
                if offset != usize::MAX {
                    area.mmap_file.offset = offset as usize;
                }
                // 注册到共享内存组
                GROUP_SHARE.lock().add_area(front_area.groupid);
                GROUP_SHARE.lock().add_area(back_area.groupid);
                // 第一轮：从小 VPN 开始，将 VPN < start_vpn 的帧迁移到 front_area
                while !area.data_frames.is_empty() {
                    let page = area.data_frames.pop_first().unwrap();
                    if page.0 >= start_vpn {
                        // 此帧属于中部或后部，放回 area
                        area.data_frames.insert(page.0, page.1);
                        break;
                    }
                    front_area.data_frames.insert(page.0, page.1);
                }
                // 第二轮：从大 VPN 开始，将 VPN >= end_vpn 的帧迁移到 back_area
                while !area.data_frames.is_empty() {
                    let page = area.data_frames.pop_last().unwrap();
                    if page.0 < end_vpn {
                        // 此帧属于中部，放回 area
                        area.data_frames.insert(page.0, page.1);
                        break;
                    }
                    back_area.data_frames.insert(page.0, page.1);
                }
                new_areas.push(front_area);
                new_areas.push(back_area);
            }
        }
        // 将拆分产生的新 area 统一插入
        for area in new_areas {
            self.areas.push(area);
        }
        // 遍历目标范围内的每个 VPN，修改硬件页表项中的权限位
        for vpn in start_vpn.0..end_vpn.0 {
            self.page_table.handle_mprotect(vpn.into(), map_perm);
        }
        // 刷新 TLB 使新权限立即生效
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
