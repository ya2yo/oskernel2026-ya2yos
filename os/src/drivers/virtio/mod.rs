#[cfg(target_arch = "loongarch64")]
mod loongarch;
#[cfg(target_arch = "riscv64")]
mod riscv;
use core::ptr::NonNull;

use alloc::slice;
use log::debug;
use virtio_drivers::{BufferDirection, Hal};
mod net;
pub use net::*;

#[cfg(target_arch = "loongarch64")]
pub use loongarch::VirtIoBlkDev2;
#[cfg(target_arch = "riscv64")]
pub use riscv::*;

use crate::{
    arch::{memory_layout::KERNEL_ADDR_OFFSET, page_table::PageTable},
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

unsafe impl Hal for VirtIoHalCMAImpl {
    fn dma_alloc(pages: usize, _direction: BufferDirection) -> (usize, NonNull<u8>) {
        match cma_alloc(pages) {
            Some(paddr_val) => {
                let paddr = paddr_val.0;
                // 计算虚拟地址。假设你的内核有固定偏移
                let vaddr_val = paddr + KERNEL_ADDR_OFFSET;
                let vaddr_ptr = NonNull::new(vaddr_val as *mut u8).expect("vaddr is null");

                (paddr, vaddr_ptr)
            }
            None => {
                panic!("DMA alloc failed");
            }
        }
    }

    unsafe fn dma_dealloc(pa: usize, _vaddr: NonNull<u8>, pages: usize) -> i32 {
        cma_dealloc(PhysAddr(pa), pages);
        0
    }

    unsafe fn mmio_phys_to_virt(
        paddr: virtio_drivers::PhysAddr,
        _size: usize,
    ) -> core::ptr::NonNull<u8> {
        let vaddr = paddr + KERNEL_ADDR_OFFSET;
        NonNull::new(vaddr as *mut u8).unwrap()
    }

    unsafe fn share(
        buffer: core::ptr::NonNull<[u8]>,
        direction: virtio_drivers::BufferDirection,
    ) -> virtio_drivers::PhysAddr {
        unsafe {
            let buffer = buffer.as_ref();
            let pages = (buffer.len() - 1 + crate::arch::memory_layout::PAGE_SIZE)
                >> crate::arch::memory_layout::PAGE_SIZE_BITS;
            let frames = cma_alloc(pages).expect("CMA alloc failed in share.");
            match direction {
                virtio_drivers::BufferDirection::DriverToDevice => {
                    let ka = KernelAddr::from(frames);
                    let va = ka.0 as *mut u8;

                    slice::from_raw_parts_mut(va, buffer.len()).copy_from_slice(buffer);
                    // frames.range_ppn.get_slice_mut()[..buffer.len()].copy_from_slice(buffer);
                }
                virtio_drivers::BufferDirection::DeviceToDriver => {}
                BufferDirection::Both => {}
            }
            frames.0
        }
    }

    unsafe fn unshare(
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
                virtio_drivers::BufferDirection::Both => {}
            }
            cma_dealloc(PhysAddr::from(paddr), pages);
        }
    }
}

/// 虚拟IO设备的错误类型
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum VirtError {
    /// There are not enough descriptors available in the virtqueue, try again later.
    QueueFull,
    /// The device is not ready.
    NotReady,
    /// The device used a different descriptor chain to the one we were expecting.
    WrongToken,
    /// The queue is already in use.
    AlreadyUsed,
    /// Invalid parameter.
    InvalidParam,
    /// Failed to alloc DMA memory.
    DmaError,
    /// I/O Error
    IoError,
    /// The request was not supported by the device.
    Unsupported,
    /// The config space advertised by the device is smaller than the driver expected.
    ConfigSpaceTooSmall,
    /// The device doesn't have any config space, but the driver expects some.
    ConfigSpaceMissing,
}
/// 虚拟设备的返回值
pub type VirtResult<T = ()> = core::result::Result<T, VirtError>;
