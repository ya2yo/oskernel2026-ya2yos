// Page Fault Handler 回调

use alloc::sync::Arc;

use crate::{
    arch::memory_layout::PAGE_SIZE,
    fs::{FilePage, FILE_PAGE_CACHE},
};

use super::group::GROUP_SHARE;
use super::{MapArea, VirtAddr, VirtPageNum};
use crate::arch::page_table::PageTable;

fn file_page_index(vma: &MapArea, va: VirtAddr) -> Option<usize> {
    vma.mmap_file.file.as_ref()?;
    let start_addr: VirtAddr = vma.vpn_range.start().into();
    va.0.checked_sub(start_addr.0)?
        .checked_add(vma.mmap_file.offset)
        .map(|offset| offset / PAGE_SIZE)
}

/// Return a file page that was loaded before taking the `MemorySet` lock.
/// A zero-length page is the page wholly beyond EOF and is therefore not
/// materializable by a mmap fault.
fn cached_file_page(va: VirtAddr, vma: &MapArea) -> Option<Arc<FilePage>> {
    let page_index = file_page_index(vma, va)?;
    let file = vma.mmap_file.file.as_ref()?;
    let page = FILE_PAGE_CACHE.get_inode(file.inode.as_ref(), page_index)?;
    (page.valid_len > 0).then_some(page)
}

/// Return a page loaded before taking the `MemorySet` write lock when it still
/// belongs to this VMA. The path and index check prevents a concurrent
/// `munmap`/replacement from installing a page prepared for an earlier VMA.
fn prepared_file_page(
    va: VirtAddr,
    vma: &MapArea,
    prepared: Option<&Arc<FilePage>>,
) -> Option<Arc<FilePage>> {
    let page = prepared?;
    let page_index = file_page_index(vma, va)?;
    let file = vma.mmap_file.file.as_ref()?;
    let path_matches = match file.inode.page_cache_path() {
        Some(path) => page.key.path.as_ref() == path.as_ref(),
        None => page.key.path.as_ref() == file.inode.path().as_str(),
    };
    (path_matches && page.key.page_index == page_index && page.valid_len > 0)
        .then(|| Arc::clone(page))
}

fn file_page_for_fault(
    va: VirtAddr,
    vma: &MapArea,
    prepared: Option<&Arc<FilePage>>,
) -> Option<Arc<FilePage>> {
    prepared_file_page(va, vma, prepared).or_else(|| cached_file_page(va, vma))
}

fn map_file_page(
    va: VirtAddr,
    page_table: &mut PageTable,
    vma: &mut MapArea,
    prepared: Option<&Arc<FilePage>>,
) -> bool {
    #[cfg(feature = "perf")]
    crate::utils::perf::record_file_page_fault();
    let Some(page) = file_page_for_fault(va, vma, prepared) else {
        return false;
    };
    let vpn: VirtPageNum = va.into();
    let frame = page.frame.clone();
    let ppn = frame.ppn;
    vma.data_frames.insert(vpn, frame);
    page_table.handle_mmap_read_page_fault(vpn, ppn, vma.map_perm, vma.mmap_flags);
    true
}

// ===================== Public Interface =========================

/// mmap写触发的lazy alocation，直接新分配帧
/// Returns true on success, false if OOM (caller should SIGSEGV).
pub fn mmap_write_page_fault(
    va: VirtAddr,
    page_table: &mut PageTable,
    vma: &mut MapArea,
    prepared: Option<&Arc<FilePage>>,
) -> bool {
    // File-backed pages are loaded by the caller before the MemorySet write
    // lock. A capacity-bypassed page is passed directly to this locked path;
    // never enter EXT4 here just because it was not retained globally.
    let cached_page = vma
        .mmap_file
        .file
        .as_ref()
        .and_then(|_| file_page_for_fault(va, vma, prepared));
    if vma.mmap_file.file.is_some() && cached_page.is_none() {
        return false;
    }
    // A MAP_SHARED writable fault can reuse the clean file page. Private
    // writable mappings allocate below so their first store is isolated from
    // the global read-only cache.
    if vma
        .mmap_flags
        .contains(crate::syscall::MmapFlags::MAP_SHARED)
        && map_file_page(va, page_table, vma, prepared)
    {
        return true;
    }

    // 映射页面,拷贝数据
    let Some(ppn) = vma.map_one(page_table, va.into()) else {
        return false;
    };
    if let Some(page) = cached_page {
        let bytes = ppn.bytes_array_mut();
        bytes.fill(0);
        bytes[..page.valid_len].copy_from_slice(&page.frame.ppn.bytes_array()[..page.valid_len]);
    }
    let vpn = va.floor();
    page_table.handle_mmap_write_page_fault(vpn, vma.map_perm, vma.mmap_flags);
    true
}
///mmap读触发的lazy alocation，查看是否有共享页可直接用，没有再直接分配
/// Returns true on success, false if OOM.
pub fn mmap_read_page_fault(
    va: VirtAddr,
    page_table: &mut PageTable,
    vma: &mut MapArea,
    prepared: Option<&Arc<FilePage>>,
) -> bool {
    let frame = GROUP_SHARE.lock().find(vma.groupid, va.into());
    if let Some(frame) = frame {
        //有现成的，直接clone,需要是cow的
        let vpn = va.into();
        let ppn = frame.ppn;
        vma.data_frames.insert(vpn, frame);

        // page_table.map(vpn, ppn, pte_flags);
        page_table.handle_mmap_read_page_fault(vpn, ppn, vma.map_perm, vma.mmap_flags);
        return true;
    }
    // MAP_PRIVATE file mappings can share clean pages between processes. The
    // page-table helper marks writable private mappings COW, so a later store
    // still gets a private copy through the normal write-protect path.
    if map_file_page(va, page_table, vma, prepared) {
        return true;
    }
    //第一次读，分配页面
    if !mmap_write_page_fault(va, page_table, vma, prepared) {
        return false;
    }
    if vma.groupid != 0 {
        GROUP_SHARE.lock().add_frame(
            vma.groupid,
            va.into(),
            vma.data_frames.get(&va.into()).unwrap().clone(),
        )
    }
    true
}
///堆触发的lazy alocation，必是写
/// Returns true on success, false if OOM.
pub fn lazy_page_fault(va: VirtAddr, page_table: &mut PageTable, vma: &mut MapArea) -> bool {
    // 仅映射页面
    vma.map_one(page_table, va.into()).is_some()
}

/// Handle a store fault on a present PTE.
///
/// This covers both real COW pages and other write-protected pages that can be
/// made writable according to the owning [`MapArea`].
pub fn write_protect_page_fault(
    va: VirtAddr,
    page_table: &mut PageTable,
    vma: &mut MapArea,
) -> bool {
    page_table.handle_write_protect_page_fault(va, vma)
}
