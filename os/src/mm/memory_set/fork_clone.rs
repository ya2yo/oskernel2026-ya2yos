//! Fork / clone address space duplication.
//!
//! Contains `from_existed_user()` which creates a new [`MemorySetInner`] by
//! cloning an existing user address space with proper COW / shared-memory
//! semantics.

use super::super::group::GROUP_SHARE;
use super::super::map_area::MapType;
use super::super::memory_set::MemorySetInner;
use super::{MapArea, MapAreaType, MapPermission, VirtAddr, VirtPageNum};
use crate::arch::memory_layout::PAGE_SIZE;
use crate::arch::page_table::PageTable;
use crate::arch::tlb::tlb_invalidate;
use crate::mm::MemorySet;
use crate::syscall::MmapFlags;
use alloc::sync::Arc;
use alloc::vec::Vec;

impl MemorySetInner {
    /// Clone a same `MemorySet`
    pub fn from_existed_user(user_space: &MemorySet) -> MemorySetInner {
        let mut memory_set = Self::new_from_kernel();
        for area in user_space.get_mut().areas.iter_mut() {
            // don't copy stack and trap
            if area.area_type == MapAreaType::Stack || area.area_type == MapAreaType::Trap {
                continue;
            }
            let mut new_area = MapArea::from_another(area);
            if area.area_type == MapAreaType::Mmap
                && !area.mmap_flags.contains(MmapFlags::MAP_SHARED)
            {
                GROUP_SHARE.lock().add_area(new_area.groupid);
            }
            // Mmap and brk are lazy allocation
            if area.area_type == MapAreaType::Mmap || area.area_type == MapAreaType::Brk {
                if area.mmap_flags.contains(MmapFlags::MAP_SHARED) {
                    let frames = area.data_frames.values().cloned().collect();
                    memory_set.push_with_given_frames(new_area, frames);
                    continue;
                }
                new_area.data_frames = area.data_frames.clone();
                for (vpn, _) in area.data_frames.iter() {
                    let vpn = *vpn;
                    user_space
                        .get_mut()
                        .page_table
                        .handle_cow_mapping_from_exited_user(vpn, &mut memory_set);
                }
                memory_set.push_lazily(new_area);
                continue;
            }
            // ELF always COW
            if area.area_type == MapAreaType::Elf {
                for vpn in area.vpn_range {
                    user_space
                        .get_mut()
                        .page_table
                        .handle_cow_mapping_from_exited_user(vpn, &mut memory_set);
                }
                new_area.data_frames = area.data_frames.clone();
                memory_set.push_lazily(new_area);
                continue;
            }
            // Map the same frames for Shm
            if area.area_type == MapAreaType::Shm {
                let frames = area.data_frames.values().cloned().collect();
                memory_set.push_with_given_frames(new_area, frames);
                continue;
            }

            // neither COW nor mmap nor shm
            memory_set.push(new_area, None);

            // copy data from another space
            for vpn in area.vpn_range {
                let src_ppn = user_space.translate(vpn).unwrap();
                let dst_ppn = memory_set.translate(vpn).unwrap();
                dst_ppn
                    .bytes_array_mut()
                    .copy_from_slice(src_ppn.bytes_array_mut());
            }
        }
        tlb_invalidate();
        memory_set
    }
}
