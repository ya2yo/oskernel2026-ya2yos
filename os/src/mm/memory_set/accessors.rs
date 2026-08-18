//! `MemorySetInner` 的页表访问、内存统计、范围校验与回收辅助函数。
//!
//! 本模块中的函数都直接操作未加锁的 [`MemorySetInner`]，可以检查或修改
//! 页表以及 VMA（`MapArea`）列表，但不会自行获取外层的
//! `MemorySet::inner` 锁。需要执行 I/O 的调用者必须先在这里创建拥有所有权
//! 的快照，释放地址空间锁之后，再调用文件系统或特殊 mmap 后端。

use alloc::{sync::Arc, vec, vec::Vec};

use super::MemorySetInner;
use crate::{
    arch::{memory_layout::PAGE_SIZE, page_table::PageTable},
    fs::{File, MmapBacking, OSFile, SEEK_CUR, SEEK_SET},
    mm::{
        user_buffer_from_kernel, FrameTracker, MapArea, MapAreaType, MapPermission, PhysPageNum,
        VPNRange, VirtAddr, VirtPageNum,
    },
    syscall::MmapFlags,
    utils::{SysErrNo, SyscallRet},
};

/// 一个共享映射中驻留页面的独立快照。
///
/// 共享映射可以由 [`OSFile`] 或实现了 [`MmapBacking`] 的对象提供后端。
/// 因此 `file` 与 `backing` 互斥：inode 文件映射填充 `file`，特殊映射
///（例如 `memfd`）填充 `backing`。`pages` 只包含已经驻留的页面；尚未访问
///的惰性页面没有物理帧，也就无需回写。
///
/// 这里有意保存 `Arc` 帧引用，使地址空间写锁释放后页面内容仍然有效，
/// 从而可以在不持有内存管理锁的情况下执行可能阻塞的 I/O。
pub(super) struct SharedMmapWriteback {
    pub(super) file: Option<Arc<OSFile>>,
    pub(super) backing: Option<Arc<dyn MmapBacking>>,
    pub(super) pages: Vec<(usize, Arc<FrameTracker>)>,
}

/// 将共享 mmap 快照中的驻留页面持久化。
///
/// 本函数不会访问 `MemorySetInner`，因此调用者可以在释放地址空间锁后执行
/// 回写。对于 inode 文件映射，函数会临时将共享文件定位到每个页面的文件
/// 偏移，写入完整页面，并在返回前恢复原文件偏移；对于特殊后端，则逐页
/// 调用 [`MmapBacking::writeback_page`]。
///
/// 如果 inode 已被删除，则视为成功的空操作，因为已经不存在可写入的文件
/// 对象。文件写入长度为零或小于预期时返回 [`SysErrNo::EIO`]；无法转换为
/// 文件接口所需有符号偏移的偏移量返回 [`SysErrNo::EOVERFLOW`]。
pub(super) fn writeback_shared_mmap_pages(snapshot: &SharedMmapWriteback) -> SyscallRet {
    if let Some(file) = snapshot.file.as_ref() {
        if file.inode.link_cnt()? == 0 {
            return Ok(0);
        }
        let saved_offset = file.lseek(0, SEEK_CUR)?;
        let saved_offset = isize::try_from(saved_offset).map_err(|_| SysErrNo::EOVERFLOW)?;
        let writeback_result = (|| -> SyscallRet {
            for (file_offset, frame) in &snapshot.pages {
                let file_offset = isize::try_from(*file_offset).map_err(|_| SysErrNo::EOVERFLOW)?;
                let mut kernel_buf = vec![0u8; PAGE_SIZE];
                kernel_buf.copy_from_slice(frame.ppn.bytes_array());
                file.lseek(file_offset, SEEK_SET)?;
                let ret = file.write(unsafe { user_buffer_from_kernel(&mut kernel_buf) })?;
                if ret == 0 || ret > PAGE_SIZE {
                    return Err(SysErrNo::EIO);
                }
            }
            Ok(0)
        })();
        let restore_result = file.lseek(saved_offset, SEEK_SET);
        return match (writeback_result, restore_result) {
            (Err(error), _) => Err(error),
            (Ok(_), Err(error)) => Err(error),
            (Ok(_), Ok(_)) => Ok(0),
        };
    }
    if let Some(backing) = snapshot.backing.as_ref() {
        for (page_index, frame) in &snapshot.pages {
            backing.writeback_page(*page_index, frame)?;
        }
    }
    Ok(0)
}

impl MemorySetInner {
    /// 收集共享映射中需要回写的驻留页面。
    ///
    /// `range` 是可选的半开 VPN 区间。指定该参数时，只处理它与每个 VMA
    /// 的交集；`munmap` 使用此模式，仅回写即将解除映射的部分，避免影响无关
    /// 映射。传入 `None` 时，处理地址空间销毁阶段的全部可写
    /// `MAP_SHARED` mmap VMA。
    ///
    /// 快照记录的是文件偏移或后端对象的页面索引，而不是虚拟地址，因此在
    /// 调用者释放 `MemorySet` guard 后仍然有效。只有 `data_frames` 中已有的
    /// 驻留帧会被收集；从未访问过的惰性页面没有需要持久化的修改内容。
    pub(super) fn collect_shared_mmap_writebacks(
        &self,
        range: Option<(VirtPageNum, VirtPageNum)>,
    ) -> Vec<SharedMmapWriteback> {
        let mut snapshots = Vec::new();
        for area in &self.areas {
            if area.area_type != MapAreaType::Mmap
                || !area.mmap_flags.contains(MmapFlags::MAP_SHARED)
                || !area.map_perm.contains(MapPermission::W)
            {
                continue;
            }
            let Some(file) = area.mmap_file.inode_file() else {
                let Some(backing) = area.mmap_file.special_backing() else {
                    continue;
                };
                let (area_start, area_end) = area.vpn_range.range();
                let (start, end) = range
                    .map(|(start, end)| (area_start.max(start), area_end.min(end)))
                    .unwrap_or((area_start, area_end));
                if start >= end {
                    continue;
                }
                let pages = area
                    .data_frames
                    .iter()
                    .filter_map(|(vpn, frame)| {
                        if *vpn < start || *vpn >= end {
                            return None;
                        }
                        let page_index = area.mmap_file.page_index(*vpn, area_start)?;
                        Some((page_index, frame.clone()))
                    })
                    .collect();
                snapshots.push(SharedMmapWriteback {
                    file: None,
                    backing: Some(backing.clone()),
                    pages,
                });
                continue;
            };
            let (area_start, area_end) = area.vpn_range.range();
            let (start, end) = range
                .map(|(start, end)| (area_start.max(start), area_end.min(end)))
                .unwrap_or((area_start, area_end));
            if start >= end {
                continue;
            }
            let pages = area
                .data_frames
                .iter()
                .filter_map(|(vpn, frame)| {
                    if *vpn < start || *vpn >= end {
                        return None;
                    }
                    let offset = vpn
                        .0
                        .checked_sub(area_start.0)?
                        .checked_mul(PAGE_SIZE)?
                        .checked_add(area.mmap_file.offset)?;
                    Some((offset, frame.clone()))
                })
                .collect();
            snapshots.push(SharedMmapWriteback {
                file: Some(file.clone()),
                backing: None,
                pages,
            });
        }
        snapshots
    }

    /// 返回硬件页表标识。
    ///
    /// 该标识是将此页表安装为当前地址空间时使用的体系结构相关值，
    /// 例如根页表标识。它对调用者是不透明的，不能在未使用目标体系结构
    /// API 的情况下将其当作物理页号解释。
    pub fn token(&self) -> usize {
        self.page_table.token()
    }

    /// 以可变方式借用底层页表。
    ///
    /// 仅供必须直接修改页表项的底层内存管理代码使用。如果操作还需要
    /// 同步更新 `areas` 元数据，应优先使用更高层的 `MemorySetInner` 方法。
    pub fn page_table_mut(&mut self) -> &mut PageTable {
        &mut self.page_table
    }

    /// 在当前 CPU 上激活此页表。
    ///
    /// 调用后，当前 hart 的地址转换将使用此地址空间。调用者负责在切换
    /// 或修改页表时遵守本模块规定的 TLB 与 `MemorySet` 锁顺序。
    pub fn activate(&self) {
        self.page_table.activate();
    }

    /// 通过此页表转换虚拟页号。
    ///
    /// 如果页表遍历找到有效的叶子项，则返回对应的物理页号；VPN 未映射
    /// 时返回 `None`。这是一次不会触发缺页异常的查询：它不检查 VMA 元数据，
    /// 也不会分配惰性页面。
    pub fn translate(&self, vpn: VirtPageNum) -> Option<PhysPageNum> {
        self.page_table.translate(vpn)
    }

    /// 返回此地址空间对应的驻留物理内存大小，单位为 KiB。
    ///
    /// 该值是所有 VMA 所跟踪驻留帧大小之和，并通过整数除法从字节转换为
    /// KiB。它统计的是已驻留的映射帧，而不是虚拟地址空间大小；由于这是
    /// 按地址空间统计的视图，被多个 VMA 通过 `Arc` 共享的帧会在每个 VMA
    /// 中分别计入。
    pub fn resident_size_kb(&self) -> usize {
        self.areas
            .iter()
            .map(|area| area.data_frames.len() * PAGE_SIZE / 1024)
            .sum()
    }

    /// 返回 `MAP_LOCKED` 映射覆盖的虚拟内存大小，单位为 KiB。
    ///
    /// 该统计依据 VMA 元数据，而不是驻留帧数量，因此惰性 `mmap` 调用完成
    /// 后，`/proc` 也会立即显示被锁定的范围。结果表示承诺保持驻留的虚拟
    /// 内存大小，而不是已经触发缺页并装入的页面数量。
    pub fn locked_size_kb(&self) -> usize {
        self.areas
            .iter()
            .filter(|area| area.mmap_flags.contains(MmapFlags::MAP_LOCKED))
            .map(|area| {
                let (start, end) = area.vpn_range.range();
                (end.0 - start.0) * PAGE_SIZE / 1024
            })
            .sum()
    }

    /// 返回所有 VMA 覆盖的虚拟地址空间总大小，单位为 KiB。
    ///
    /// 每个 VMA 都使用半开 VPN 区间表示，因此计算结果包含每个区域完整的
    /// 页对齐范围。该值统计虚拟地址覆盖范围，与页面是否驻留无关，也不包含
    /// VMA 之间未映射的空洞。
    pub fn virtual_size_kb(&self) -> usize {
        self.areas
            .iter()
            .map(|area| {
                let (start, end) = area.vpn_range.range();
                (end.0 - start.0) * PAGE_SIZE / 1024
            })
            .sum()
    }

    /// 删除所有用户 VMA，并清除对应的页表项。
    ///
    /// 除了丢弃区域元数据和页表映射，该操作还会重置 mmap 统计值与分配提示
    /// 地址。共享 mmap 的回写由持有锁的 `MemorySet` handle 在单独阶段完成，
    /// 因为文件系统 I/O 可能阻塞，不能在地址空间状态锁定期间执行。调用者
    /// 必须在调用本方法前安排所需的 TLB shootdown 并保留必要的物理帧引用。
    pub fn recycle_data_pages(&mut self) -> SyscallRet {
        self.areas.clear();
        self.page_table.clear();
        self.total_mmap_size = 0;
        self.mmap_hint = crate::arch::memory_layout::MMAP_TOP;
        Ok(0)
    }

    /// 检查半开 VPN 区间是否被具有所需权限的 VMA 完整覆盖。
    ///
    /// 区间不能包含 VMA 之间的空洞，并且其中每个区域都必须包含
    /// `wanted_map_perm` 中的全部权限位。本函数只检查 VMA 元数据，不要求每个
    /// 页面都存在驻留 PTE，也不会触发惰性分配。调用者应传入非空区间；字节
    /// 范围的调用者会在进入本函数前处理长度为零的情况。
    pub(super) fn check_user_range(
        &self,
        vpn_range: VPNRange,
        wanted_map_perm: MapPermission,
    ) -> bool {
        log::trace!("[check_valid_user_vpn_range]");
        let mut current_vpn = vpn_range.start();
        let end_vpn = vpn_range.end();

        for area in self.areas.iter() {
            if area.vpn_range.end() <= current_vpn {
                continue;
            }
            if !area.vpn_range.contains_vpn(current_vpn) {
                log::debug!(
                    "[check_valid_user_vpn_range] can't find area with vpn {:#x}",
                    current_vpn.0
                );
                self.areas.iter().for_each(|area| {
                    log::debug!(
                        "[check_valid_user_vpn_range] area: {:#x?}, {:?}",
                        area.vpn_range,
                        area.map_perm
                    );
                });
                return false;
            }
            if !area.map_perm.contains(wanted_map_perm) {
                log::debug!(
                    "[check_valid_user_vpn_range] vpn {:#x} has wrong map permission: {:?}, wanted: {:?}",
                    current_vpn.0,
                    area.map_perm,
                    wanted_map_perm
                );
                return false;
            }
            current_vpn = core::cmp::min(area.vpn_range.end(), end_vpn);

            if current_vpn >= end_vpn {
                break;
            }
        }

        if current_vpn < end_vpn {
            log::debug!(
                "[check_valid_user_vpn_range] reach end prematurely at {:#x}, want {:#x}",
                current_vpn.0,
                end_vpn.0
            );
            return false;
        }
        true
    }
}
