//! Page-table access, accounting and teardown helpers for `MemorySetInner`.

use alloc::vec;

use super::MemorySetInner;
use crate::{
    arch::{memory_layout::PAGE_SIZE, page_table::PageTable},
    fs::{File, OSFile, SEEK_CUR, SEEK_SET},
    mm::{
        read_user_bytes_direct_into, user_buffer_from_kernel, MapArea, MapAreaType, MapPermission,
        PhysPageNum, VPNRange, VirtAddr, VirtPageNum,
    },
    syscall::MmapFlags,
    utils::{SysErrNo, SyscallRet},
};

const MMAP_WRITEBACK_CHUNK_SIZE: usize = 0x10000;

/// Write one contiguous run of resident shared-mmap pages back to its file.
fn writeback_resident_segment(
    token: usize,
    area: &MapArea,
    file: &OSFile,
    start_vpn: VirtPageNum,
    end_vpn: VirtPageNum,
) -> SyscallRet {
    let map_base: usize = VirtAddr::from(area.vpn_range.start()).into();
    let start_addr: usize = VirtAddr::from(start_vpn).into();
    let segment_len = end_vpn
        .0
        .checked_sub(start_vpn.0)
        .and_then(|pages| pages.checked_mul(PAGE_SIZE))
        .ok_or(SysErrNo::EOVERFLOW)?;
    let mapped_offset = start_addr.checked_sub(map_base).ok_or(SysErrNo::EFAULT)?;
    let file_base = area
        .mmap_file
        .offset
        .checked_add(mapped_offset)
        .ok_or(SysErrNo::EOVERFLOW)?;

    let mut written = 0;
    while written < segment_len {
        let chunk_len = MMAP_WRITEBACK_CHUNK_SIZE.min(segment_len - written);
        let mut kernel_buf = vec![0u8; chunk_len];
        let user_addr = start_addr.checked_add(written).ok_or(SysErrNo::EOVERFLOW)?;
        read_user_bytes_direct_into(token, user_addr, &mut kernel_buf).ok_or(SysErrNo::EFAULT)?;

        let file_offset = file_base.checked_add(written).ok_or(SysErrNo::EOVERFLOW)?;
        let file_offset = isize::try_from(file_offset).map_err(|_| SysErrNo::EOVERFLOW)?;
        file.lseek(file_offset, SEEK_SET)?;
        let ret = file.write(unsafe { user_buffer_from_kernel(&mut kernel_buf) })?;
        if ret == 0 || ret > chunk_len {
            return Err(SysErrNo::EIO);
        }
        written += ret;
    }
    Ok(0)
}

/// Write every resident shared-mmap page back and restore the shared open-file
/// description offset even when a writeback step fails.
fn writeback_shared_mmap_area(token: usize, area: &MapArea, file: &OSFile) -> SyscallRet {
    // Match munmap's existing behavior: an unlinked file has no persistent
    // pathname target to update during final address-space teardown.
    if file.inode.link_cnt()? == 0 {
        return Ok(0);
    }

    let saved_offset = file.lseek(0, SEEK_CUR)?;
    let saved_offset = isize::try_from(saved_offset).map_err(|_| SysErrNo::EOVERFLOW)?;
    let writeback_result = (|| -> SyscallRet {
        let mut segment_start: Option<VirtPageNum> = None;
        let mut previous: Option<VirtPageNum> = None;

        for vpn in area
            .data_frames
            .keys()
            .copied()
            .filter(|vpn| area.vpn_range.contains_vpn(*vpn))
        {
            if let Some(prev) = previous {
                let expected = prev.0.checked_add(1).ok_or(SysErrNo::EOVERFLOW)?;
                if vpn.0 != expected {
                    let start = segment_start.ok_or(SysErrNo::EFAULT)?;
                    writeback_resident_segment(token, area, file, start, VirtPageNum(expected))?;
                    segment_start = Some(vpn);
                }
            } else {
                segment_start = Some(vpn);
            }
            previous = Some(vpn);
        }

        if let (Some(start), Some(last)) = (segment_start, previous) {
            let end = VirtPageNum(last.0.checked_add(1).ok_or(SysErrNo::EOVERFLOW)?);
            writeback_resident_segment(token, area, file, start, end)?;
        }
        Ok(0)
    })();

    // Do not use `?` before this restore: OSFile offset is shared by every
    // descriptor referring to the same open-file description.
    let restore_result = file.lseek(saved_offset, SEEK_SET);
    match (writeback_result, restore_result) {
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
        (Ok(_), Ok(_)) => Ok(0),
    }
}

impl MemorySetInner {
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

    /// Write back shared writable mmap pages, then clear all areas and page-table entries.
    pub fn recycle_data_pages(&mut self) -> SyscallRet {
        let token = self.page_table.token();
        let mut first_writeback_error = None;
        for area in self.areas.iter() {
            if area.area_type == MapAreaType::Mmap
                && area.mmap_flags.contains(MmapFlags::MAP_SHARED)
                && area.map_perm.contains(MapPermission::W)
            {
                if let Some(file) = area.mmap_file.file.as_deref() {
                    let writeback_result = writeback_shared_mmap_area(token, area, file);
                    if first_writeback_error.is_none() {
                        first_writeback_error = writeback_result.err();
                    }
                }
            }
        }
        self.areas.clear();
        self.page_table.clear();
        self.total_mmap_size = 0;
        match first_writeback_error {
            Some(error) => Err(error),
            None => Ok(0),
        }
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
