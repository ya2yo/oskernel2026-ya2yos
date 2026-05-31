// Page Fault Handler 回调

use alloc::sync::Arc;
use log::{debug, warn};

use crate::{
    arch::{memory_layout::PAGE_SIZE, tlb::tlb_invalidate},
    fs::{File, SEEK_CUR, SEEK_SET},
};

use super::group::GROUP_SHARE;
use super::{translated_byte_buffer, MapArea, UserBuffer, VirtAddr};
use crate::arch::page_table::PageTable;

///mmap写触发的lazy alocation，直接新分配帧
pub fn mmap_write_page_fault(va: VirtAddr, page_table: &mut PageTable, vma: &mut MapArea) {
    debug!("[mmap_write_page_fault] va={:?}", va);
    // 映射页面,拷贝数据
    vma.map_one(page_table, va.into());
    if vma.mmap_file.file.is_none() {
        return;
    }
    let file = vma.mmap_file.file.clone().unwrap();
    let old_offset = file.lseek(0, SEEK_CUR).unwrap();
    let start_addr: VirtAddr = vma.vpn_range.start().into();
    let va = va.0;

    /*
    debug!(
        "va={:x},start_addr={:x},vma.offset={:x}",
        va, start_addr.0, vma.mmap_file.offset
    );
    */

    file.lseek(
        (va - start_addr.0 + vma.mmap_file.offset) as isize,
        SEEK_SET,
    )
    .expect("mmap_write_page_fault should not fail");
    file.read(UserBuffer {
        buffers: translated_byte_buffer(page_table.token(), va as *const u8, PAGE_SIZE).unwrap(),
    })
    .expect("mmap_write_page_fault should not fail");
    file.lseek(old_offset as isize, SEEK_SET)
        .expect("mmap_write_page_fault should not fail");
    //设置为cow
    let vpn = VirtAddr::from(va).floor();
    page_table.handle_mmap_write_page_fault(vpn, vma.map_perm);
}
///mmap读触发的lazy alocation，查看是否有共享页可直接用，没有再直接分配
pub fn mmap_read_page_fault(va: VirtAddr, page_table: &mut PageTable, vma: &mut MapArea) {
    debug!("[mmap_read_page_fault] va={:?}", va);
    let frame = GROUP_SHARE.lock().find(vma.groupid, va.into());
    if let Some(frame) = frame {
        //有现成的，直接clone,需要是cow的
        let vpn = va.into();
        let ppn = frame.ppn;
        vma.data_frames.insert(vpn, frame);

        // page_table.map(vpn, ppn, pte_flags);
        page_table.handle_mmap_read_page_fault(vpn, ppn, vma.map_perm);
    } else {
        //第一次读，分配页面
        mmap_write_page_fault(va, page_table, vma);
        if vma.groupid != 0 {
            GROUP_SHARE.lock().add_frame(
                vma.groupid,
                va.into(),
                vma.data_frames.get(&va.into()).unwrap().clone(),
            )
        }
    }
}
///堆触发的lazy alocation，必是写
pub fn lazy_page_fault(va: VirtAddr, page_table: &mut PageTable, vma: &mut MapArea) {
    // 仅映射页面
    vma.map_one(page_table, va.into());
}

///copy on write
pub fn cow_page_fault(va: VirtAddr, page_table: &mut PageTable, vma: &mut MapArea) -> bool {
    page_table.handle_cow_page_fault(va, vma)
}
