//! Page-table access, accounting and teardown helpers for `MemorySetInner`.

use alloc::vec;

use super::MemorySetInner;
use crate::{
    arch::{memory_layout::PAGE_SIZE, page_table::PageTable},
    fs::{File, SEEK_CUR, SEEK_SET},
    mm::{
        read_user_bytes_direct_into, user_buffer_from_kernel, MapAreaType, MapPermission,
        PhysPageNum, VPNRange, VirtAddr, VirtPageNum,
    },
    syscall::MmapFlags,
    utils::SyscallRet,
};

const MMAP_WRITEBACK_CHUNK_SIZE: usize = 0x10000;

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
        for area in self.areas.iter_mut() {
            if area.area_type == MapAreaType::Mmap
                && area.mmap_flags.contains(MmapFlags::MAP_SHARED)
                && area.map_perm.contains(MapPermission::W)
            {
                if let Some(file) = area.mmap_file.file.clone() {
                    let addr: VirtAddr = area.vpn_range.start().into();
                    let mapped_len: usize = area
                        .vpn_range
                        .into_iter()
                        .filter(|vpn| area.data_frames.contains_key(&vpn))
                        .count()
                        * PAGE_SIZE;
                    let off = file.lseek(0, SEEK_CUR)?;
                    let mut written = 0;
                    while written < mapped_len {
                        let chunk_len = MMAP_WRITEBACK_CHUNK_SIZE.min(mapped_len - written);
                        let mut kernel_buf = vec![0u8; chunk_len];
                        if read_user_bytes_direct_into(
                            self.page_table.token(),
                            addr.0 as usize + written,
                            &mut kernel_buf,
                        )
                        .is_none()
                        {
                            break;
                        }
                        let buf = unsafe { user_buffer_from_kernel(&mut kernel_buf) };
                        file.lseek((area.mmap_file.offset + written) as isize, SEEK_SET)?;
                        let ret = file.write(buf)?;
                        if ret == 0 {
                            break;
                        }
                        written += ret;
                    }
                    file.lseek(off as isize, SEEK_SET)?;
                }
            }
        }
        self.areas.clear();
        self.page_table.clear();
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
                return false;
            }
            if !area.map_perm.contains(wanted_map_perm) {
                log::error!(
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
            log::error!(
                "[check_valid_user_vpn_range] reach end prematurely at {:#x}, want {:#x}",
                current_vpn.0,
                end_vpn.0
            );
            return false;
        }
        true
    }
}
