//! Page-table access, accounting and teardown helpers for `MemorySetInner`.

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

/// A snapshot of resident shared-mmap pages. Filesystem I/O is performed from
/// this snapshot after the owning `MemorySet` lock has been released.
pub(super) struct SharedMmapWriteback {
    pub(super) file: Option<Arc<OSFile>>,
    pub(super) backing: Option<Arc<dyn MmapBacking>>,
    pub(super) pages: Vec<(usize, Arc<FrameTracker>)>,
}

/// Write a shared-mmap snapshot without touching a MemorySet lock.
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
    /// Snapshot resident shared-mmap pages for writeback. The returned frames
    /// keep their contents alive while filesystem I/O runs without the
    /// MemorySet lock.
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

    /// Return the hardware page-table token.
    pub fn token(&self) -> usize {
        self.page_table.token()
    }

    /// Borrow the underlying page table mutably.
    ///
    /// This is for low-level memory-management code that must edit page-table
    /// entries directly. Prefer higher-level `MemorySetInner` methods when the
    /// operation also needs to keep `areas` metadata in sync.
    pub fn page_table_mut(&mut self) -> &mut PageTable {
        &mut self.page_table
    }

    /// Activate this page table on the current CPU.
    pub fn activate(&self) {
        self.page_table.activate();
    }

    /// Translate a VPN through this page table.
    pub fn translate(&self, vpn: VirtPageNum) -> Option<PhysPageNum> {
        self.page_table.translate(vpn)
    }

    /// Resident physical memory in KiB.
    pub fn resident_size_kb(&self) -> usize {
        self.areas
            .iter()
            .map(|area| area.data_frames.len() * PAGE_SIZE / 1024)
            .sum()
    }

    /// Return pages covered by `MAP_LOCKED` mappings.
    ///
    /// This follows VMA metadata rather than resident frames so `/proc` shows
    /// the lock immediately after a lazy `mmap` call.
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

    /// Virtual address space size in KiB.
    pub fn virtual_size_kb(&self) -> usize {
        self.areas
            .iter()
            .map(|area| {
                let (start, end) = area.vpn_range.range();
                (end.0 - start.0) * PAGE_SIZE / 1024
            })
            .sum()
    }

    /// Clear all user VM areas and page-table entries.
    ///
    /// Shared-mmap writeback is intentionally performed by the locked handle
    /// in a separate phase, because filesystem I/O may sleep.
    pub fn recycle_data_pages(&mut self) -> SyscallRet {
        self.areas.clear();
        self.page_table.clear();
        self.total_mmap_size = 0;
        self.mmap_hint = crate::arch::memory_layout::MMAP_TOP;
        Ok(0)
    }

    /// Check that a VPN range is fully covered by user-accessible areas with permissions.
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
