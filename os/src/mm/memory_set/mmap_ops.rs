//! `mmap`、`munmap`、`mprotect` 和共享内存操作逻辑。
//!
//! 这些是 [`MemorySetInner`] 中实现用户态虚拟内存操作的方法，负责匿名映射和
//! 文件映射的建立、解除映射、修改保护属性和挂载共享内存。

use super::{
    FrameTracker, MapArea, MapAreaType, MapPermission, PhysAddr, UserBuffer, VPNRange, VirtAddr,
    VirtPageNum,
};
use crate::arch::memory_layout::{
    MAX_MMAP_SIZE, MMAP_TOP, PAGE_SIZE, PAGE_SIZE_BITS, USER_SPACE_SIZE,
};
use crate::arch::tlb::{instruction_fence, tlb_invalidate};
use crate::fs::{File, Inode, OSFile, OpenFlags};
use crate::mm::group::GROUP_SHARE;
use crate::mm::map_area::MapType;
use crate::mm::memory_set::MemorySetInner;
use crate::syscall::MmapFlags;
use crate::utils::{SysErrNo, SyscallRet};
use alloc::vec;
use alloc::{string::String, sync::Arc, vec::Vec};
use log::{debug, warn};

/// 判断区域是否属于可由 mmap 系列操作管理的 VMA。
fn is_mmap_vma(area: &MapArea) -> bool {
    area.area_type == MapAreaType::Mmap
        || (area.area_type == MapAreaType::Stack && area.mmap_flags.contains(MmapFlags::MAP_STACK))
}

impl MemorySetInner {
    /// 返回文件映射 VMA 对应的底层 inode 和文件页索引。
    ///
    /// 调用者可以在释放 MemorySet 锁后使用该快照执行可能阻塞的文件系统读取。
    pub fn mmap_file_page_info(&self, vpn: VirtPageNum) -> Option<(Arc<dyn Inode>, usize)> {
        let area = self
            .areas
            .iter()
            .find(|area| area.area_type == MapAreaType::Mmap && area.vpn_range.contains_vpn(vpn))?;
        let file = area.mmap_file.file.as_ref()?;
        let page_offset = (vpn.0 - area.vpn_range.start().0)
            .checked_mul(PAGE_SIZE)?
            .checked_add(area.mmap_file.offset)?;
        Some((file.inode.clone(), page_offset / PAGE_SIZE))
    }

    /// 收集 fork 期间实例化共享映射所需的全部文件页。
    ///
    /// 持有 MemorySet 读锁时只复制元数据和 inode 的引用计数指针；实际的 EXT4
    /// 读取在调用者释放锁后的阶段执行。
    pub fn shared_file_page_info(&self) -> Vec<(Arc<dyn Inode>, usize)> {
        let mut requests = Vec::new();
        for area in &self.areas {
            if !area.mmap_flags.contains(MmapFlags::MAP_SHARED) || !is_mmap_vma(area) {
                continue;
            }
            let Some(file) = area.mmap_file.file.as_ref() else {
                continue;
            };
            for vpn in area.vpn_range {
                let page_offset = (vpn.0 - area.vpn_range.start().0)
                    .saturating_mul(PAGE_SIZE)
                    .saturating_add(area.mmap_file.offset);
                requests.push((file.inode.clone(), page_offset / PAGE_SIZE));
            }
        }
        requests
    }

    /// 将给定的物理页挂载到当前地址空间中的共享内存区域。
    ///
    /// 当 `addr` 为 0 时自动选择 mmap 区域内的空闲地址；指定地址的挂载目前
    /// 尚未实现。
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

    /// 从当前地址空间解除一个 SysV 共享内存映射。
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

    /// 建立一个匿名或文件支持的用户态虚拟内存映射。
    ///
    /// 对普通映射，函数在地址空间中寻找空闲范围并创建延迟分配的 VMA；对固定
    /// 映射，使用调用者指定的地址，并根据标志处理冲突映射。成功时返回映射起始
    /// 地址，失败时返回 0，由系统调用层转换为相应的 errno。
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
            // MAP_FIXED 会替换请求范围内已有的映射。如果范围超出旧 VMA，直接
            // 追加新 VMA 会留下重叠条目；缺页查找可能先命中旧条目（通常是
            // PROT_NONE），从而遮蔽新的替换映射。
            if flags.contains(MmapFlags::MAP_FIXED) {
                // brk 的起止位置由进程状态单独维护，不能像 mmap VMA 一样由
                // munmap 截断。拒绝覆盖 brk，避免留下重叠 VMA 和不一致的 brk。
                if self.areas.iter().any(|area| {
                    if area.area_type != MapAreaType::Brk {
                        return false;
                    }
                    let (l, r) = area.vpn_range.range();
                    l < end_vpn && start_vpn < r
                }) {
                    return 0;
                }
                let _ = self.munmap(addr, len);
            }
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
            // MAP_FIXED / MAP_FIXED_NOREPLACE 使用指定地址，不计入 mmap 总量
            return addr;
        }
        // 拒绝超过单进程 mmap 限额的分配。没有此检查时，失控的 mmap（例如
        // glibc 在字符设备上调用 ungetc）可能无限分配虚拟空间，并在后续延迟
        // 缺页时耗尽物理内存。
        if self.total_mmap_size + len > MAX_MMAP_SIZE {
            debug!(
                "[mmap] ENOMEM: total_mmap_size={}, request={}, max={}",
                self.total_mmap_size, len, MAX_MMAP_SIZE
            );
            return 0; // 向 sys_mmap 表示失败，由其返回 ENOMEM
        }
        let addr = self.find_insert_addr(MMAP_TOP, len);
        if addr == 0 {
            return 0; // 未找到可用空间
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

    /// 解除指定范围内的 mmap 映射，并按需要截断或拆分 VMA。
    ///
    /// 解除范围与现有 VMA 的交集，释放对应页表项和物理页；对于被部分覆盖的
    /// VMA，保留范围外部分及其已分配页。
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

            // 共享映射的回写由 `MemorySet::munmap` 在本锁保护的元数据和页表更新
            // 之前完成。
            // 卸载交集范围内的所有页
            for vpn in VPNRange::new(unmap_start, unmap_end) {
                area.unmap_one(&mut self.page_table, vpn);
            }
            // 固定映射创建时不计入 mmap 预算，因此移除固定映射时也不能从
            // 预算中扣除其页数。
            if !area
                .mmap_flags
                .intersects(MmapFlags::MAP_FIXED | MmapFlags::MAP_FIXED_NOREPLACE)
            {
                let trimmed = (unmap_end.0 - unmap_start.0) * PAGE_SIZE;
                self.total_mmap_size = self.total_mmap_size.saturating_sub(trimmed);
            }

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
                self.push_lazily(right_area);
            }
            tlb_invalidate();
        }
        Ok(0)
    }

    /// 验证内存建议操作的范围是否完全由已有 VMA 覆盖。
    pub fn validate_madvise_range(&self, addr: usize, len: usize) -> SyscallRet {
        if addr % PAGE_SIZE != 0 {
            return Err(SysErrNo::EINVAL);
        }
        let end_addr = addr.checked_add(len).ok_or(SysErrNo::EINVAL)?;
        if len == 0 {
            return Ok(0);
        }
        if VirtAddr::try_from(addr).is_none() || VirtAddr::try_from(end_addr - 1).is_none() {
            return Err(SysErrNo::EINVAL);
        }

        let start_vpn = VirtAddr::from(addr).floor();
        let end_vpn = VirtAddr::from(end_addr).ceil();
        let mut cursor = start_vpn;
        while cursor < end_vpn {
            let Some(area) = self.areas.iter().find(|area| {
                let (start, end) = area.vpn_range.range();
                start <= cursor && cursor < end
            }) else {
                return Err(SysErrNo::ENOMEM);
            };
            cursor = area.vpn_range.end();
        }
        Ok(0)
    }

    /// 丢弃映射范围内的驻留页，同时保留 VMA 元数据。
    ///
    /// `MADV_DONTNEED` 通过从延迟分配的 VMA 中移除驻留页实现。之后再次访问时，
    /// 缺页处理会重新分配填零的匿名页，或从底层文件载入页面。固定 ELF 映射被
    /// 有意保留，因为其缺页路径没有足够的文件元数据来重建原始段内容。
    pub fn discard_madvise_pages(&mut self, addr: usize, len: usize) -> SyscallRet {
        self.validate_madvise_range(addr, len)?;
        if len == 0 {
            return Ok(0);
        }
        let end_addr = addr.checked_add(len).ok_or(SysErrNo::EINVAL)?;
        let start_vpn = VirtAddr::from(addr).floor();
        let end_vpn = VirtAddr::from(end_addr).ceil();

        for area in self.areas.iter_mut() {
            let (area_start, area_end) = area.vpn_range.range();
            let discard_start = area_start.max(start_vpn);
            let discard_end = area_end.min(end_vpn);
            if discard_start >= discard_end
                || !(area.area_type == MapAreaType::Brk || is_mmap_vma(area))
                || area.mmap_flags.contains(MmapFlags::MAP_SHARED)
            {
                continue;
            }

            for vpn in VPNRange::new(discard_start, discard_end) {
                area.unmap_one(&mut self.page_table, vpn);
            }
        }
        tlb_invalidate();
        Ok(0)
    }

    /// 将完整的 mmap VMA 移动到新的空闲范围，同时保留所有驻留页的内容。
    ///
    /// Rust 分配器使用 `MREMAP_MAYMOVE` 扩大其后备映射。解除源映射后再重建
    /// VMA 会丢失分配器仍在使用的内容，因此先构造目标映射，待所有驻留页复制
    /// 完成后再提交源映射的拆除。对于 `MAP_SHARED` 映射，驻留页会深复制到新
    /// 的物理帧，延迟页仍保持延迟状态；旧的 GROUP_SHARE 条目会成为孤儿，但
    /// 由于旧 VPN 范围已经解除映射，它们不会再被访问。
    pub fn mremap_maymove(
        &mut self,
        old_addr: usize,
        old_len: usize,
        new_len: usize,
        new_addr: usize,
        fixed: bool,
    ) -> SyscallRet {
        let old_end_addr = old_addr.checked_add(old_len).ok_or(SysErrNo::EINVAL)?;
        let old_start_vpn = VirtAddr::from(old_addr).floor();
        let old_end_vpn = VirtAddr::from(old_end_addr).ceil();
        let Some(mut old_idx) = self.areas.iter().position(|area| {
            (is_mmap_vma(area) || area.area_type == MapAreaType::Shm)
                && area.vpn_range.start() <= old_start_vpn
                && old_end_vpn <= area.vpn_range.end()
        }) else {
            return Err(SysErrNo::EFAULT);
        };

        // 将 VMA 截取为精确的 [old_start_vpn, old_end_vpn) 范围。
        let area_start_vpn = self.areas[old_idx].vpn_range.start();
        let area_end_vpn = self.areas[old_idx].vpn_range.end();

        // 如果 VMA 在请求范围之前开始，则先拆出前部。
        if area_start_vpn < old_start_vpn {
            let mut front_area = MapArea::from_another(&self.areas[old_idx]);
            front_area.vpn_range = VPNRange::new(area_start_vpn, old_start_vpn);
            let front_keys: Vec<VirtPageNum> = self.areas[old_idx]
                .data_frames
                .range(..old_start_vpn)
                .map(|(k, _)| *k)
                .collect();
            for k in front_keys {
                if let Some(frame) = self.areas[old_idx].data_frames.remove(&k) {
                    front_area.data_frames.insert(k, frame);
                }
            }
            self.areas[old_idx].vpn_range = VPNRange::new(old_start_vpn, area_end_vpn);
            // VMA 起点后移，因此调整文件偏移，使 (va - start + offset) 仍然
            // 对应正确的文件页。
            self.areas[old_idx].mmap_file.offset +=
                (old_start_vpn.0 - area_start_vpn.0) * PAGE_SIZE;
            if front_area.groupid != 0 {
                GROUP_SHARE.lock().add_area(front_area.groupid);
            }
            // Keep the split visible until the source VMA is removed or
            // resized. Merging here would restore the original range and
            // make `old_idx` refer to the wrong logical VMA.
            let front_idx = self.insert_area_sorted_unmerged(front_area);
            if front_idx <= old_idx {
                old_idx += 1;
            }
        }

        // 如果 VMA 超出请求范围，则拆出尾部。
        if old_end_vpn < area_end_vpn {
            let mut tail_area = MapArea::from_another(&self.areas[old_idx]);
            tail_area.vpn_range = VPNRange::new(old_end_vpn, area_end_vpn);
            // 尾部区域起点晚于源 VMA，因此调整其文件偏移。
            tail_area.mmap_file.offset += (old_end_vpn.0 - old_start_vpn.0) * PAGE_SIZE;
            let tail_keys: Vec<VirtPageNum> = self.areas[old_idx]
                .data_frames
                .range(old_end_vpn..)
                .map(|(k, _)| *k)
                .collect();
            for k in tail_keys {
                if let Some(frame) = self.areas[old_idx].data_frames.remove(&k) {
                    tail_area.data_frames.insert(k, frame);
                }
            }
            self.areas[old_idx].vpn_range = VPNRange::new(old_start_vpn, old_end_vpn);
            if tail_area.groupid != 0 {
                GROUP_SHARE.lock().add_area(tail_area.groupid);
            }
            self.insert_area_sorted_unmerged(tail_area);
        }

        // MREMAP_FIXED 路径：验证并准备目标范围。
        if fixed {
            let new_end = new_addr.checked_add(new_len).ok_or(SysErrNo::EINVAL)?;
            if new_end > USER_SPACE_SIZE
                || VirtAddr::try_from(new_addr).is_none()
                || VirtAddr::try_from(new_end - 1).is_none()
            {
                return Err(SysErrNo::EINVAL);
            }
            // 源范围和目标范围不能重叠。
            if new_addr < old_end_addr && old_addr < new_end {
                return Err(SysErrNo::EINVAL);
            }
            // 解除目标范围内已有的映射。
            self.munmap(new_addr, new_len)?;
            // 重新查找旧区域，因为 munmap 可能改变 self.areas 中的索引。
            let Some(reidx) = self.areas.iter().position(|area| {
                (is_mmap_vma(area) || area.area_type == MapAreaType::Shm)
                    && area.vpn_range.start() <= old_start_vpn
                    && old_end_vpn <= area.vpn_range.end()
            }) else {
                return Err(SysErrNo::EFAULT);
            };
            old_idx = reidx;
        }

        let old_flags = self.areas[old_idx].mmap_flags;
        if old_flags.contains(MmapFlags::MAP_SHARED_VALIDATE) {
            return Err(SysErrNo::ENOSYS);
        }

        let old_len = (old_end_vpn.0 - old_start_vpn.0) * PAGE_SIZE;
        if new_len == old_len && !fixed {
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

        // 选择目标时将源 VMA 保留在障碍集合中，确保复制过程中两个范围不会重叠。
        let dest_addr = if fixed {
            new_addr
        } else {
            self.find_insert_addr(MMAP_TOP, new_len)
        };
        if dest_addr == 0 {
            return Err(SysErrNo::ENOMEM);
        }
        let new_end_addr = dest_addr.checked_add(new_len).ok_or(SysErrNo::ENOMEM)?;
        let new_start_vpn = VirtAddr::from(dest_addr).floor();
        let new_end_vpn = VirtAddr::from(new_end_addr).ceil();
        let new_page_count = new_end_vpn.0 - new_start_vpn.0;

        let mut new_area = MapArea::from_another(&self.areas[old_idx]);
        new_area.vpn_range = VPNRange::new(new_start_vpn, new_end_vpn);
        new_area
            .mmap_flags
            .remove(MmapFlags::MAP_FIXED | MmapFlags::MAP_FIXED_NOREPLACE);
        if new_area.groupid != 0 {
            GROUP_SHARE.lock().add_area(new_area.groupid);
        }

        let copy_page_count = (old_end_vpn.0 - old_start_vpn.0).min(new_page_count);
        for page_offset in 0..copy_page_count {
            let old_vpn = VirtPageNum(old_start_vpn.0 + page_offset);
            let Some(old_ppn) = self.page_table.translate(old_vpn) else {
                continue;
            };

            // 在分配目标页期间固定源帧。通常第一次查找即可成功；备用查找还可以
            // 容忍较早的 VMA 拆分将有效 PTE 的帧跟踪器留在相邻区域的情况。
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

        // 只有在目标映射完整复制旧 VMA 的所有驻留页后才提交变更；延迟页继续
        // 保持延迟状态。
        let mut old_area = self.areas.remove(old_idx);
        old_area.unmap(&mut self.page_table);
        self.push_lazily(new_area);
        self.total_mmap_size = new_total_mmap_size;
        tlb_invalidate();
        Ok(dest_addr)
    }

    /// 在不改变起始地址的情况下调整完整 VMA 的大小。
    ///
    /// 不带 `MREMAP_MAYMOVE` 时，Linux 只能将映射扩展到完全空闲的相邻范围。
    /// VMA 仍保持延迟分配状态，因此扩展只修改元数据；新访问的页沿用现有的
    /// mmap 缺页路径（包括共享文件页缓存处理）。缩小操作委托给 `munmap`，
    /// 使驻留的共享页经过正常的回写路径。
    pub fn mremap_in_place(
        &mut self,
        old_addr: usize,
        old_len: usize,
        new_len: usize,
    ) -> SyscallRet {
        let old_end_addr = old_addr.checked_add(old_len).ok_or(SysErrNo::EINVAL)?;
        let old_start_vpn = VirtAddr::from(old_addr).floor();
        let old_end_vpn = VirtAddr::from(old_end_addr).ceil();
        let Some(mut old_idx) = self.areas.iter().position(|area| {
            (is_mmap_vma(area) || area.area_type == MapAreaType::Shm)
                && area.vpn_range.start() <= old_start_vpn
                && old_end_vpn <= area.vpn_range.end()
        }) else {
            return Err(SysErrNo::EFAULT);
        };

        // 将 VMA 截取为精确的 [old_start_vpn, old_end_vpn) 范围。
        let area_start_vpn = self.areas[old_idx].vpn_range.start();
        let area_end_vpn = self.areas[old_idx].vpn_range.end();

        // 如果 VMA 在请求范围之前开始，则先拆出前部。
        if area_start_vpn < old_start_vpn {
            let mut front_area = MapArea::from_another(&self.areas[old_idx]);
            front_area.vpn_range = VPNRange::new(area_start_vpn, old_start_vpn);
            let front_keys: Vec<VirtPageNum> = self.areas[old_idx]
                .data_frames
                .range(..old_start_vpn)
                .map(|(k, _)| *k)
                .collect();
            for k in front_keys {
                if let Some(frame) = self.areas[old_idx].data_frames.remove(&k) {
                    front_area.data_frames.insert(k, frame);
                }
            }
            self.areas[old_idx].vpn_range = VPNRange::new(old_start_vpn, area_end_vpn);
            self.areas[old_idx].mmap_file.offset +=
                (old_start_vpn.0 - area_start_vpn.0) * PAGE_SIZE;
            if front_area.groupid != 0 {
                GROUP_SHARE.lock().add_area(front_area.groupid);
            }
            // Keep the split visible until the in-place resize has checked
            // the tail for conflicts; coalescing would hide that blocker.
            let front_idx = self.insert_area_sorted_unmerged(front_area);
            if front_idx <= old_idx {
                old_idx += 1;
            }
        }

        // 如果 VMA 超出请求范围，则拆出尾部，使下面的扩展检查能够检测冲突并
        // 返回 ENOMEM；当 VMA 的其余部分阻塞原地扩展时，Linux 也采用此行为。
        if old_end_vpn < area_end_vpn {
            let mut tail_area = MapArea::from_another(&self.areas[old_idx]);
            tail_area.vpn_range = VPNRange::new(old_end_vpn, area_end_vpn);
            tail_area.mmap_file.offset += (old_end_vpn.0 - old_start_vpn.0) * PAGE_SIZE;
            let tail_keys: Vec<VirtPageNum> = self.areas[old_idx]
                .data_frames
                .range(old_end_vpn..)
                .map(|(k, _)| *k)
                .collect();
            for k in tail_keys {
                if let Some(frame) = self.areas[old_idx].data_frames.remove(&k) {
                    tail_area.data_frames.insert(k, frame);
                }
            }
            self.areas[old_idx].vpn_range = VPNRange::new(old_start_vpn, old_end_vpn);
            if tail_area.groupid != 0 {
                GROUP_SHARE.lock().add_area(tail_area.groupid);
            }
            self.insert_area_sorted_unmerged(tail_area);
        }

        let old_len = (old_end_vpn.0 - old_start_vpn.0) * PAGE_SIZE;
        if new_len == old_len {
            return Ok(old_addr);
        }
        if new_len < old_len {
            let trim_addr = old_addr.checked_add(new_len).ok_or(SysErrNo::EINVAL)?;
            self.munmap(trim_addr, old_len - new_len)?;
            return Ok(old_addr);
        }

        let new_end_addr = old_addr.checked_add(new_len).ok_or(SysErrNo::EINVAL)?;
        if new_end_addr > USER_SPACE_SIZE
            || VirtAddr::try_from(old_addr).is_none()
            || VirtAddr::try_from(new_end_addr - 1).is_none()
        {
            return Err(SysErrNo::EINVAL);
        }
        let new_end_vpn = VirtAddr::from(new_end_addr).ceil();
        let overlaps = self.areas.iter().enumerate().any(|(idx, area)| {
            if idx == old_idx {
                return false;
            }
            let (start, end) = area.vpn_range.range();
            start < new_end_vpn && old_end_vpn < end
        });
        if overlaps {
            return Err(SysErrNo::ENOMEM);
        }

        let old_flags = self.areas[old_idx].mmap_flags;
        let old_was_accounted =
            !old_flags.intersects(MmapFlags::MAP_FIXED | MmapFlags::MAP_FIXED_NOREPLACE);
        let new_total_mmap_size = if old_was_accounted {
            self.total_mmap_size
                .checked_add(new_len - old_len)
                .ok_or(SysErrNo::ENOMEM)?
        } else {
            self.total_mmap_size
        };
        if new_total_mmap_size > MAX_MMAP_SIZE {
            return Err(SysErrNo::ENOMEM);
        }

        self.areas[old_idx].vpn_range = VPNRange::new(old_start_vpn, new_end_vpn);
        self.total_mmap_size = new_total_mmap_size;
        tlb_invalidate();
        Ok(old_addr)
    }

    /// 修改一段虚拟地址空间的访问权限（`mprotect` 核心逻辑）。
    ///
    /// 此函数完成两件事：
    /// 1. **逻辑段（MapArea）拆分**：将现有 area 按 `[start_vpn, end_vpn)` 范围切分，
    ///    确保范围内的区域获得新权限，范围外的区域保持原权限不变。
    /// 2. **页表项更新**：遍历范围内的每个 VPN，调用 `handle_mprotect` 修改硬件页表项的权限位。
    ///
    /// # 参数
    /// - `start_vpn`, `end_vpn`: 目标虚拟页号范围（左闭右开）。
    /// - `map_perm`: 要设置的新权限。
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
                continue;
            // 情况2：area 左侧在范围外，右侧在范围内
            // 例: area=[2,6) 目标=[4,9) → 拆为 [2,4)原权限 + [4,6)新权限
            } else if start < start_vpn && end > start_vpn && end <= end_vpn {
                // new_area: 右半部，获得新权限
                let mut new_area = MapArea::from_another(area);
                new_area.map_perm = map_perm;
                new_area.vpn_range = VPNRange::new(start_vpn, end);
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
            self.push_lazily(area);
        }
        // 遍历目标范围内的每个 VPN，修改硬件页表项中的权限位
        for vpn in start_vpn.0..end_vpn.0 {
            self.page_table.handle_mprotect(vpn.into(), map_perm);
        }
        // 刷新 TLB 使新权限立即生效
        tlb_invalidate();
        if map_perm.contains(MapPermission::X) {
            instruction_fence();
        }
    }
}
