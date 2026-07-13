#[cfg(target_arch = "loongarch64")]
pub mod loongarch;
#[cfg(target_arch = "riscv64")]
pub mod riscv;
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

/// 将 PCI 读出的物理地址规范到 QEMU 分配的 MMIO 窗口（与 `pci.rs` 中 set_bar 一致）
#[cfg(target_arch = "loongarch64")]
fn loongarch_mmio_paddr(paddr: usize) -> usize {
    use crate::arch::memory_layout::{VIRTIO_PCI_MMIO_BASE, VIRTIO_PCI_MMIO_SIZE};

    if paddr >= VIRTIO_PCI_MMIO_BASE && paddr < VIRTIO_PCI_MMIO_BASE + VIRTIO_PCI_MMIO_SIZE {
        return paddr;
    }
    // 误读为 0x1_0000_0000_40000 等形式时，低 20 位常对应 BAR 内偏移
    if paddr >= 0x1_0000_0000_0000 {
        let lo = paddr as u32 as usize;
        if lo < VIRTIO_PCI_MMIO_SIZE {
            return VIRTIO_PCI_MMIO_BASE + lo;
        }
    }
    paddr
}

#[cfg(not(target_arch = "loongarch64"))]
fn loongarch_mmio_paddr(paddr: usize) -> usize {
    paddr
}

unsafe impl Hal for VirtIoHalCMAImpl {
    fn dma_alloc(pages: usize, _direction: BufferDirection) -> (usize, NonNull<u8>) {
        match cma_alloc(pages) {
            Some(paddr_val) => {
                let paddr = paddr_val.0;
                let vaddr_val = paddr + KERNEL_ADDR_OFFSET;
                let vaddr_ptr = NonNull::new(vaddr_val as *mut u8).expect("vaddr is null");

                // `virtio_drivers::Hal` requires DMA regions to start zeroed. In
                // particular, a non-zero virtqueue avail/used index makes the device
                // consume stale descriptors before the driver has submitted one.
                let bytes = pages
                    .checked_mul(crate::arch::memory_layout::PAGE_SIZE)
                    .expect("DMA allocation size overflow");
                unsafe {
                    core::ptr::write_bytes(vaddr_ptr.as_ptr(), 0, bytes);
                }

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
        let pa = loongarch_mmio_paddr(paddr);
        let vaddr = pa + KERNEL_ADDR_OFFSET;
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
