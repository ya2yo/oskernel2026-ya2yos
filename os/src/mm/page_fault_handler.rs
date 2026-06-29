// Page Fault Handler 回调

use alloc::sync::Arc;
use alloc::vec;
use log::{debug, warn};

use crate::{
    arch::{memory_layout::PAGE_SIZE, tlb::tlb_invalidate},
    fs::{File, SEEK_CUR, SEEK_SET},
};

use super::group::GROUP_SHARE;
use super::{user_buffer_from_kernel, write_user_bytes_direct, MapArea, UserBuffer, VirtAddr};
use crate::arch::page_table::PageTable;

///mmap写触发的lazy alocation，直接新分配帧
/// Returns true on success, false if OOM (caller should SIGSEGV).
pub fn mmap_write_page_fault(va: VirtAddr, page_table: &mut PageTable, vma: &mut MapArea) -> bool {
    // debug!("[mmap_write_page_fault] va={:?}", va);
    // 映射页面,拷贝数据
    if vma.map_one(page_table, va.into()).is_none() {
        return false;
    }
    if vma.mmap_file.file.is_none() {
        return true;
    }
    let file = vma.mmap_file.file.clone().unwrap();
    let old_offset = file.lseek(0, SEEK_CUR).unwrap();
    let start_addr: VirtAddr = vma.vpn_range.start().into();
    let va = va.0;

    file.lseek(
        (va - start_addr.0 + vma.mmap_file.offset) as isize,
        SEEK_SET,
    )
    .expect("mmap_write_page_fault should not fail");
    let mut kernel_buf = vec![0u8; PAGE_SIZE];
    let buf = unsafe { user_buffer_from_kernel(&mut kernel_buf) };
    file.read(buf)
        .expect("mmap_write_page_fault should not fail");
    write_user_bytes_direct(page_table.token(), va as usize, &kernel_buf);
    file.lseek(old_offset as isize, SEEK_SET)
        .expect("mmap_write_page_fault should not fail");
    let vpn = VirtAddr::from(va).floor();
    page_table.handle_mmap_write_page_fault(vpn, vma.map_perm, vma.mmap_flags);
    true
}
///mmap读触发的lazy alocation，查看是否有共享页可直接用，没有再直接分配
/// Returns true on success, false if OOM.
pub fn mmap_read_page_fault(va: VirtAddr, page_table: &mut PageTable, vma: &mut MapArea) -> bool {
    // debug!("[mmap_read_page_fault] va={:?}", va);
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
    //第一次读，分配页面
    if !mmap_write_page_fault(va, page_table, vma) {
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
