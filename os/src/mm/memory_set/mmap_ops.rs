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
    lazy_page_fault, mmap_file_page_beyond_eof, mmap_read_page_fault, mmap_write_page_fault,
    write_protect_page_fault,
};
use crate::syscall::MmapFlags;
use crate::trap::trap_types::*;
use crate::utils::{SysErrNo, SyscallRet};
use alloc::vec;
use alloc::{string::String, sync::Arc, vec::Vec};
use log::{debug, warn};

const MMAP_WRITEBACK_CHUNK_SIZE: usize = 0x10000; // 64KB

// pthread stacks are allocated with mmap(MAP_STACK), but the area type is
// kept as Stack so page faults use the regular stack lazy-allocation path.
// They are nevertheless ordinary dynamic VMAs and must be reclaimed by
// munmap.  The fixed process stack is a Stack without MAP_STACK and remains
// excluded.
fn is_dynamic_mmap_stack(area: &MapArea) -> bool {
    area.area_type == MapAreaType::Stack && area.mmap_flags.contains(MmapFlags::MAP_STACK)
}

fn is_mmap_vma(area: &MapArea) -> bool {
    area.area_type == MapAreaType::Mmap || is_dynamic_mmap_stack(area)
}

impl MemorySetInner {
    /// Check the Linux SIGBUS condition for a file-backed mmap fault.
    pub fn mmap_file_page_beyond_eof(&self, vpn: VirtPageNum) -> bool {
        self.areas
            .iter()
            .filter(|area| area.area_type == MapAreaType::Mmap)
            .find(|area| area.vpn_range.contains_vpn(vpn))
            .is_some_and(|area| mmap_file_page_beyond_eof(vpn.into(), area))
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

    /// Detach a SysV shared memory mapping from the current address space.
    pub fn shm_detach(&mut self, addr: usize) -> SyscallRet {
        if addr % PAGE_SIZE != 0 {
            return Err(SysErrNo::EINVAL);
        }
        let start_vpn = VirtAddr::from(addr).floor();
        let Some(idx) = self.areas.iter().position(|area| {
            area.area_type == MapAreaType::Shm && area.vpn_range.start() == start_vpn
        }) else {
            return Err(SysErrNo::EINVAL);
        };
        let mut area = self.areas.remove(idx);
        area.unmap(&mut self.page_table);
        tlb_invalidate();
        Ok(0)
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
                self.mprotect(start_vpn, end_vpn, map_perm, file, off, Some(flags));
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
        if start_vpn >= end_vpn {
            return Err(SysErrNo::EINVAL);
        }
        while let Some((idx, area)) = self
            .areas
            .iter_mut()
            .enumerate()
            .filter(|(_, area)| is_mmap_vma(area))
            .find(|(_, area)| {
                let (start, end) = area.vpn_range.range();
                start < end_vpn && end > start_vpn
            })
        {
            let (area_start, area_end) = area.vpn_range.range();
            // 计算 munmap 范围与此 area 的交集
            let unmap_start = area_start.max(start_vpn);
            let unmap_end = area_end.min(end_vpn);

            // 共享映射脏页写回（仅实际卸载的部分）
            if area.mmap_flags.contains(MmapFlags::MAP_SHARED)
                && area.map_perm.contains(MapPermission::W)
                && area.mmap_file.file.is_some()
            {
                let file = area.mmap_file.file.clone().unwrap();
                if file.inode.link_cnt()? > 0 {
                    let mut wb_range: Vec<(VirtPageNum, VirtPageNum)> = Vec::new();
                    for vpn in unmap_start.0..unmap_end.0 {
                        let vpn = VirtPageNum(vpn);
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
                    }
                    let off = file.lseek(0, SEEK_CUR).unwrap();
                    let map_base: usize = VirtAddr::from(area.vpn_range.start()).into();
                    let file_base = area.mmap_file.offset;
                    for (wb_vpn_start, wb_vpn_end) in wb_range {
                        let start_addr: usize = VirtAddr::from(wb_vpn_start).into();
                        let mapped_len: usize = (wb_vpn_end.0 - wb_vpn_start.0) * PAGE_SIZE;
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
            // 卸载交集范围内的所有页
            for vpn in VPNRange::new(unmap_start, unmap_end) {
                area.unmap_one(&mut self.page_table, vpn);
            }
            let trimmed = (unmap_end.0 - unmap_start.0) * PAGE_SIZE;
            self.total_mmap_size = self.total_mmap_size.saturating_sub(trimmed);

            if area_start >= start_vpn && area_end <= end_vpn {
                // 情况1：area 完全在卸载范围内 → 删除整个 area
                self.areas.remove(idx);
            } else if area_start < start_vpn && area_end <= end_vpn {
                // 情况2：area 左侧伸出卸载范围 → 截断右端
                area.vpn_range = VPNRange::new(area_start, start_vpn);
            } else if area_start >= start_vpn && area_end > end_vpn {
                // 情况3：area 右侧伸出卸载范围 → 截断左端
                area.vpn_range = VPNRange::new(end_vpn, area_end);
            } else {
                // 情况4：area 完全包围卸载范围 → 拆分为左右两个 area
                // 左部 [area_start, start_vpn) 保留在原 area
                // 右部 [end_vpn, area_end) 创建新 area
                let mut right_area = MapArea::from_another(area);
                right_area.vpn_range = VPNRange::new(end_vpn, area_end);
                // 迁移属于右部的 data_frames
                let right_keys: Vec<VirtPageNum> =
                    area.data_frames.range(end_vpn..).map(|(k, _)| *k).collect();
                for k in right_keys {
                    if let Some(frame) = area.data_frames.remove(&k) {
                        right_area.data_frames.insert(k, frame);
                    }
                }
                area.vpn_range = VPNRange::new(area_start, start_vpn);
                self.areas.push(right_area);
            }
            tlb_invalidate();
        }
        Ok(0)
    }

    /// Move a complete private mmap VMA to a new free range while retaining
    /// the contents of every resident page.
    ///
    /// `MREMAP_MAYMOVE` is used by Rust's allocator to grow its backing
    /// mappings. Recreating the VMA after unmapping the source loses the
    /// allocator's live contents, so construct the destination first and
    /// commit the source teardown only after every resident page is copied.
    /// Shared mappings are deliberately left unsupported here: `GROUP_SHARE`
    /// currently indexes frames by absolute VPN, so moving one side of a
    /// shared mapping requires a separate representation change.
    pub fn mremap_maymove(
        &mut self,
        old_addr: usize,
        old_len: usize,
        new_len: usize,
    ) -> SyscallRet {
        let old_end_addr = old_addr.checked_add(old_len).ok_or(SysErrNo::EINVAL)?;
        let old_start_vpn = VirtAddr::from(old_addr).floor();
        let old_end_vpn = VirtAddr::from(old_end_addr).ceil();
        let old_range = VPNRange::new(old_start_vpn, old_end_vpn);
        let Some(old_idx) = self.areas.iter().position(|area| {
            area.area_type == MapAreaType::Mmap && area.vpn_range.range() == old_range.range()
        }) else {
            return Err(SysErrNo::EFAULT);
        };

        let old_flags = self.areas[old_idx].mmap_flags;
        if !old_flags.contains(MmapFlags::MAP_PRIVATE)
            || old_flags.intersects(MmapFlags::MAP_SHARED | MmapFlags::MAP_SHARED_VALIDATE)
        {
            return Err(SysErrNo::ENOSYS);
        }

        let old_len = (old_end_vpn.0 - old_start_vpn.0) * PAGE_SIZE;
        if new_len == old_len {
            return Ok(old_addr);
        }
        let old_was_accounted =
            !old_flags.intersects(MmapFlags::MAP_FIXED | MmapFlags::MAP_FIXED_NOREPLACE);
        let base_mmap_size = if old_was_accounted {
            self.total_mmap_size
                .checked_sub(old_len)
                .ok_or(SysErrNo::EINVAL)?
        } else {
            self.total_mmap_size
        };
        let Some(new_total_mmap_size) = base_mmap_size.checked_add(new_len) else {
            return Err(SysErrNo::ENOMEM);
        };
        if new_total_mmap_size > MAX_MMAP_SIZE {
            return Err(SysErrNo::ENOMEM);
        }

        // Keep the source VMA in the obstacle set while selecting a target,
        // so the two ranges can never overlap during the copy.
        let new_addr = self.find_insert_addr(MMAP_TOP, new_len);
        if new_addr == 0 {
            return Err(SysErrNo::ENOMEM);
        }
        let new_end_addr = new_addr.checked_add(new_len).ok_or(SysErrNo::ENOMEM)?;
        let new_start_vpn = VirtAddr::from(new_addr).floor();
        let new_end_vpn = VirtAddr::from(new_end_addr).ceil();
        let new_page_count = new_end_vpn.0 - new_start_vpn.0;

        let mut new_area = MapArea::from_another(&self.areas[old_idx]);
        new_area.vpn_range = VPNRange::new(new_start_vpn, new_end_vpn);
        new_area
            .mmap_flags
            .remove(MmapFlags::MAP_FIXED | MmapFlags::MAP_FIXED_NOREPLACE);

        let copy_page_count = (old_end_vpn.0 - old_start_vpn.0).min(new_page_count);
        for page_offset in 0..copy_page_count {
            let old_vpn = VirtPageNum(old_start_vpn.0 + page_offset);
            let Some(old_ppn) = self.page_table.translate(old_vpn) else {
                continue;
            };

            // Pin the source frame while allocating the destination. Normally
            // the first lookup succeeds. The fallback also tolerates an older
            // VMA split that left a valid PTE's tracker in a neighboring area.
            let source_frame = self.areas[old_idx]
                .data_frames
                .get(&old_vpn)
                .filter(|frame| frame.ppn == old_ppn)
                .cloned()
                .or_else(|| {
                    self.areas.iter().find_map(|area| {
                        area.data_frames
                            .values()
                            .find(|frame| frame.ppn == old_ppn)
                            .cloned()
                    })
                });
            let Some(source_frame) = source_frame else {
                new_area.unmap(&mut self.page_table);
                tlb_invalidate();
                return Err(SysErrNo::EFAULT);
            };

            let new_vpn = VirtPageNum(new_start_vpn.0 + page_offset);
            let Some(new_ppn) = new_area.map_one(&mut self.page_table, new_vpn) else {
                new_area.unmap(&mut self.page_table);
                tlb_invalidate();
                return Err(SysErrNo::ENOMEM);
            };
            new_ppn
                .bytes_array_mut()
                .copy_from_slice(source_frame.ppn.bytes_array());
        }

        // Commit only after the destination has a complete copy of all pages
        // that were resident in the old VMA. Lazy pages remain lazy.
        let mut old_area = self.areas.remove(old_idx);
        old_area.unmap(&mut self.page_table);
        self.areas.push(new_area);
        self.total_mmap_size = new_total_mmap_size;
        tlb_invalidate();
        Ok(new_addr)
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
        mmap_flags: Option<MmapFlags>,
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
                if mmap_flags.is_some() {
                    area.mmap_file.replace(file.clone(), offset);
                }
                if let Some(flags) = mmap_flags {
                    area.mmap_flags = flags;
                }
                continue;
            // 情况2：area 左侧在范围外，右侧在范围内
            // 例: area=[2,6) 目标=[4,9) → 拆为 [2,4)原权限 + [4,6)新权限
            } else if start < start_vpn && end > start_vpn && end <= end_vpn {
                // new_area: 右半部，获得新权限
                let mut new_area = MapArea::from_another(area);
                new_area.map_perm = map_perm;
                new_area.vpn_range = VPNRange::new(start_vpn, end);
                if mmap_flags.is_some() {
                    new_area.mmap_file.replace(file.clone(), offset);
                }
                if let Some(flags) = mmap_flags {
                    new_area.mmap_flags = flags;
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
                if mmap_flags.is_some() {
                    new_area.mmap_file.replace(file.clone(), offset);
                }
                if let Some(flags) = mmap_flags {
                    new_area.mmap_flags = flags;
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
                if mmap_flags.is_some() {
                    area.mmap_file.replace(file.clone(), offset);
                }
                if let Some(flags) = mmap_flags {
                    area.mmap_flags = flags;
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
    pub fn handle_page_fault(&mut self, vpn: VirtPageNum, scause: Trap) -> bool {
        if self.handle_not_present_page_fault(vpn, scause) {
            return true;
        }
        self.handle_write_protect_page_fault(vpn, scause)
    }

    fn handle_not_present_page_fault(&mut self, vpn: VirtPageNum, scause: Trap) -> bool {
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
            // A file VMA may legally cover bytes past EOF, but faulting a
            // complete page beyond EOF is SIGBUS, never a demand-zero page.
            if mmap_file_page_beyond_eof(vpn.into(), area) {
                return false;
            }
            let ok = match scause {
                Trap::Exception(Exception::LoadPageFault) => {
                    area.map_perm.contains(MapPermission::R)
                        && mmap_read_page_fault(vpn.into(), &mut self.page_table, area)
                }
                Trap::Exception(Exception::FetchInstructionPageFault) => {
                    area.map_perm.contains(MapPermission::X)
                        && mmap_read_page_fault(vpn.into(), &mut self.page_table, area)
                }
                Trap::Exception(Exception::PagePrivilegeIllegal) => {
                    if area
                        .map_perm
                        .intersects(MapPermission::R | MapPermission::X)
                    {
                        mmap_read_page_fault(vpn.into(), &mut self.page_table, area)
                    } else {
                        area.map_perm.contains(MapPermission::W)
                            && mmap_write_page_fault(vpn.into(), &mut self.page_table, area)
                    }
                }
                _ => {
                    area.map_perm.contains(MapPermission::W)
                        && mmap_write_page_fault(vpn.into(), &mut self.page_table, area)
                }
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
            let allowed = match scause {
                Trap::Exception(Exception::LoadPageFault) => {
                    area.map_perm.contains(MapPermission::R)
                }
                Trap::Exception(Exception::FetchInstructionPageFault) => {
                    area.map_perm.contains(MapPermission::X)
                }
                Trap::Exception(Exception::PagePrivilegeIllegal) => area
                    .map_perm
                    .intersects(MapPermission::R | MapPermission::W | MapPermission::X),
                _ => area.map_perm.contains(MapPermission::W),
            };
            return allowed && lazy_page_fault(vpn.into(), &mut self.page_table, area);
        }
        false
    }

    fn handle_write_protect_page_fault(&mut self, vpn: VirtPageNum, scause: Trap) -> bool {
        // Only store/page-modify faults can be fixed by COW or write permission
        // restoration. Load/fetch permission faults must remain SIGSEGV.
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
            if write_protect_page_fault(vpn.into(), &mut self.page_table, area) {
                return true;
            }
        }
        false
    }
}
