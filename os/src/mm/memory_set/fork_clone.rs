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
use crate::fs::FilePageKey;
use crate::mm::{page_fault_handler, MemorySet};
use crate::syscall::MmapFlags;
use alloc::sync::Arc;
use alloc::vec::Vec;

fn is_dynamic_mmap_stack(area: &MapArea) -> bool {
    area.area_type == MapAreaType::Stack && area.mmap_flags.contains(MmapFlags::MAP_STACK)
}

fn is_mmap_vma(area: &MapArea) -> bool {
    area.area_type == MapAreaType::Mmap || is_dynamic_mmap_stack(area)
}

impl MemorySetInner {
    /// Clone a same `MemorySet`
    pub fn from_existed_user(user_space: &MemorySet) -> MemorySetInner {
        let mut memory_set = Self::new_from_kernel();

        // Loading file-backed MAP_SHARED pages can sleep on EXT4. Do it
        // before taking the parent's MemorySet write lock; the locked phase
        // below only allocates anonymous frames and installs cached frames.
        let prepared_shared_pages = user_space.prefetch_shared_file_pages();

        user_space.with_mut(|u| {
            // Pre-fault MAP_SHARED areas: lazy mmap pages need backing frames
            // allocated before forking, otherwise parent and child would each
            // independently allocate their own frames on page fault, breaking
            // MAP_SHARED semantics. File pages were prefetched above, so this
            // loop does not enter the filesystem while the lock is held.
            let areas_ptr: *mut Vec<MapArea> = &mut u.areas;
            let pt_ptr: *mut PageTable = &mut u.page_table;
            for area in unsafe { &mut *areas_ptr }.iter_mut() {
                if !is_mmap_vma(area) || !area.mmap_flags.contains(MmapFlags::MAP_SHARED) {
                    continue;
                }
                let pt = unsafe { &mut *pt_ptr };
                for vpn in area.vpn_range {
                    if !area.data_frames.contains_key(&vpn) {
                        if area.mmap_file.file.is_none() {
                            // MAP_ANONYMOUS: just allocate a zeroed frame
                            area.map_one(pt, vpn); // ignore OOM — lazy fault later
                        } else {
                            // file-backed: use the write-fault handler to
                            // install the page loaded before the MemorySet
                            // lock was acquired.
                            let va = VirtAddr::from(vpn);
                            let prepared = area.mmap_file.file.as_ref().and_then(|file| {
                                let page_offset = (vpn.0 - area.vpn_range.start().0)
                                    .checked_mul(PAGE_SIZE)?
                                    .checked_add(area.mmap_file.offset)?;
                                let path = file
                                    .inode
                                    .page_cache_path()
                                    .unwrap_or_else(|| file.inode.path().into());
                                prepared_shared_pages.get(&FilePageKey {
                                    path,
                                    page_index: page_offset / PAGE_SIZE,
                                })
                            });
                            page_fault_handler::mmap_write_page_fault(va, pt, area, prepared);
                        }
                    }
                }
            }

            for area in u.areas.iter_mut() {
                // The fixed task stack and trap context are rebuilt by
                // `clone_process`. A MAP_STACK VMA is instead a dynamic mmap
                // and must be inherited like every other mmap area.
                if (area.area_type == MapAreaType::Stack && !is_dynamic_mmap_stack(area))
                    || area.area_type == MapAreaType::Trap
                {
                    continue;
                }
                let mut new_area = MapArea::from_another(area);
                if is_mmap_vma(area) && area.mmap_flags.contains(MmapFlags::MAP_SHARED) {
                    // 子进程继承 MAP_SHARED 的 groupid，增加引用计数后才允许
                    // 父子在后续 lazy fault 中从 GROUP_SHARE 找到同一共享帧。
                    GROUP_SHARE.lock().add_area(new_area.groupid);
                }
                // Mmap and brk are lazy allocation
                if is_mmap_vma(area) || area.area_type == MapAreaType::Brk {
                    if area.mmap_flags.contains(MmapFlags::MAP_SHARED) {
                        let frames = area.data_frames.values().cloned().collect();
                        memory_set.push_with_given_frames(new_area, frames);
                        continue;
                    }
                    new_area.data_frames = area.data_frames.clone();
                    // Iterate the full vpn_range for Brk areas: the parent may have
                    // PTEs for VPNs not tracked in data_frames (e.g. after brk
                    // shrink→grow cycles).  For each such VPN we must create a child
                    // COW PTE so the child sees the parent's heap data instead of
                    // a zero page from a subsequent lazy fault.
                    let vpns: Vec<_> = if area.area_type == MapAreaType::Brk {
                        area.vpn_range.into_iter().collect()
                    } else {
                        area.data_frames.keys().copied().collect()
                    };
                    for vpn in vpns {
                        if u.page_table.translate(vpn).is_some() {
                            u.page_table
                                .handle_cow_mapping_from_exited_user(vpn, &mut memory_set);
                        }
                    }
                    memory_set.push_lazily(new_area);
                    continue;
                }
                // ELF always COW
                if area.area_type == MapAreaType::Elf {
                    for vpn in area.vpn_range {
                        u.page_table
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
                memory_set.push(new_area, None).ok();

                // copy data from another space
                for vpn in area.vpn_range {
                    let src_ppn = u.page_table.translate(vpn).unwrap();
                    let dst_ppn = memory_set.translate(vpn).unwrap();
                    dst_ppn
                        .bytes_array_mut()
                        .copy_from_slice(src_ppn.bytes_array_mut());
                }
            }
        });
        tlb_invalidate();
        memory_set
    }
}
