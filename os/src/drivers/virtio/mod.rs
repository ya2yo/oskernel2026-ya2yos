mod blk;
mod pci;
mod net;
use alloc::slice;
pub use blk::*;
use log::debug;
pub use pci::*;
use virtio_drivers::Hal;

use crate::{
    arch::page_table::PageTable,
    drivers::DevError,
    mm::{cma_alloc, cma_dealloc, KernelAddr, PhysAddr, PhysPageNum, VirtAddr},
    task::current_token,
};

#[allow(dead_code)]
const fn as_dev_err(e: virtio_drivers::Error) -> DevError {
    use virtio_drivers::Error::*;
    match e {
        NotReady => DevError::Again,
        AlreadyUsed => DevError::AlreadyExists,
        InvalidParam => DevError::InvalidParam,
        DmaError => DevError::NoMemory,
        IoError => DevError::Io,
        _ => DevError::BadState,
    }
}

// TatlinOS有了CMA功能后的改进实现
// 也许会存在并发问题？

pub struct VirtIoHalCMAImpl;

impl Hal for VirtIoHalCMAImpl {
    fn dma_alloc(pages: usize) -> usize {
        match cma_alloc(pages) {
            Some(addr) => addr.0,
            None => 0,
        }
    }

    fn dma_dealloc(pa: usize, pages: usize) -> i32 {
        cma_dealloc(PhysAddr(pa), pages);
        0
    }

    fn phys_to_virt(addr: usize) -> usize {
        KernelAddr::from(PhysAddr::from(addr)).0
    }

    fn share(
        buffer: core::ptr::NonNull<[u8]>,
        direction: virtio_drivers::BufferDirection,
    ) -> virtio_drivers::PhysAddr {
        unsafe {
            let buffer = buffer.as_ref();
            let pages = (buffer.len() - 1 + crate::arch::memory_layout::PAGE_SIZE)
                >> crate::arch::memory_layout::PAGE_SIZE_BITS;

            let frames = cma_alloc(pages).unwrap();

            match direction {
                virtio_drivers::BufferDirection::DriverToDevice => {
                    let ka = KernelAddr::from(frames);
                    let va = ka.0 as *mut u8;

                    slice::from_raw_parts_mut(va, buffer.len()).copy_from_slice(buffer);
                    // frames.range_ppn.get_slice_mut()[..buffer.len()].copy_from_slice(buffer);
                }
                virtio_drivers::BufferDirection::DeviceToDriver => {}
            }
            frames.0
        }
    }

    fn unshare(
        paddr: virtio_drivers::PhysAddr,
        mut buffer: core::ptr::NonNull<[u8]>,
        direction: virtio_drivers::BufferDirection,
    ) {
        unsafe {
            let buffer = buffer.as_mut();
            let ka = KernelAddr::from(PhysAddr::from(paddr));
            let va = ka.0 as *const u8;
            let ppn_start = PhysAddr::from(paddr).floor();
            let ppn_end = PhysAddr::from(paddr + buffer.len()).ceil();
            let pages = ppn_end.0 - ppn_start.0;
            match direction {
                virtio_drivers::BufferDirection::DeviceToDriver => {
                    let src = slice::from_raw_parts(va, buffer.len());
                    buffer.copy_from_slice(src);
                }
                virtio_drivers::BufferDirection::DriverToDevice => {}
            }
            cma_dealloc(PhysAddr::from(paddr), pages);
        }
    }
}
