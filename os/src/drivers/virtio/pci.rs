use crate::arch::memory_layout::{KERNEL_ADDR_OFFSET, MMIO_MAP_OFFSET};
use crate::arch::page_table::get_token_from_regs;
use crate::drivers::{BaseDriver, BlockDriver, DevResult, DeviceType};
use crate::mm::{self, cma_alloc, VirtAddr, VirtPageNum, KERNEL_SPACE};
use log::{debug, warn};
use spin::Mutex;
use virtio_drivers::device::blk::VirtIOBlk;
use virtio_drivers::transport::mmio::VirtIOHeader;
use virtio_drivers::transport::pci::bus::{
    BarInfo, Cam, Command, DeviceFunction, MemoryBarType, PciRoot,
};
use virtio_drivers::transport::pci::PciTransport;
use virtio_drivers::transport::{DeviceStatus, Transport};
use virtio_drivers::{Hal, PhysAddr, PAGE_SIZE};

use super::as_dev_err;

pub struct VirtIoBlkDev2<H: Hal> {
    inner: Mutex<VirtIOBlk<H, PciTransport>>,
}

unsafe impl<H: Hal> Send for VirtIoBlkDev2<H> {}
unsafe impl<H: Hal> Sync for VirtIoBlkDev2<H> {}

const DEVICE: u8 = 1;

fn pci_config_read(bus: u8, device: u8, func: u8, offset: u8) -> u32 {
    let ecam_base: usize = 0x20000000 + KERNEL_ADDR_OFFSET;
    let addr: usize = ecam_base
        + ((bus as usize) << 20
            | (device as usize) << 15
            | (func as usize) << 12
            | (offset as usize));
    let addr = addr as *mut u32;

    return unsafe { *addr };
}

fn pci_config_write(bus: u8, device: u8, func: u8, offset: u8, val: u32) {
    let ecam_base: usize = 0x20000000 + KERNEL_ADDR_OFFSET;
    let addr: usize = ecam_base
        + ((bus as usize) << 20
            | (device as usize) << 15
            | (func as usize) << 12
            | (offset as usize));
    let addr = addr as *mut u32;

    unsafe { *addr = val };
}

fn read_status() -> u16 {
    let x: u32 = pci_config_read(0, DEVICE, 0, 4);
    let hig: u16 = (x >> 16) as u16;
    return hig;
}

fn write_status(s: u16) {
    let x: u32 = pci_config_read(0, DEVICE, 0, 4);
    let y = x & 0xFFFF;
    let mut s32 = s as u32;
    s32 <<= 16;
    s32 |= y;
    pci_config_write(0, DEVICE, 0, 4, s32);
}
impl<H: Hal> VirtIoBlkDev2<H> {
    pub fn new() -> Self {
        // 在进入此函数前，没有任何对PCI的MMIO寄存器的操作
        unsafe {
            // 根据龙芯的直接翻译规则，pa+KERNEL_ADDR_OFFSET=va
            let root = 0x20000000 + KERNEL_ADDR_OFFSET;
            let mut root = PciRoot::new(root as *mut u8, Cam::Ecam);
            let func = DeviceFunction {
                bus: 0,
                device: DEVICE,
                function: 0,
            };

            let mut i = 0;
            // 读取设备信息，注意，这些信息实际上不会被用到
            // 这个循环是用来获取有哪些可用的func的，实际的func会硬编码在上面
            for bus in 0..128 {
                // 当bus大于128时会出现问题，故以128结尾
                for device in 0..32 {
                    for func in 0..8 {
                        // debug!("iter ({},{},{})", bus, device, func);
                        let dev_id = pci_config_read(bus, device, func, 0x00);
                        let vendor_id: u16 = (dev_id & 0xFFFF) as u16;
                        let device_id: u16 = (dev_id >> 16) as u16;
                        if vendor_id != 0xFFFF || device_id != 0xFFFF {
                            println!(
                                "found: vendor_id={:#x}, device_id={:#x}, ({},{},{})",
                                vendor_id, device_id, bus, device, func
                            );
                        }
                    }
                }
            }
            println!("end of find");

            let dev_id = pci_config_read(0, DEVICE, 0, 0x00);
            let vendor_id: u16 = (dev_id & 0xFFFF) as u16;
            let device_id: u16 = (dev_id >> 16) as u16;
            println!("vendor_id={:#x}, device_id={:#x}", vendor_id, device_id);
            // 启用设备(0,DEVICE,0)
            let mut command_reg = pci_config_read(0, DEVICE, 0, 0x04);
            debug!("get cmd_reg={:#x}", command_reg);
            command_reg |= 0x02; // 设置bit1（Memory Space Enable）
            debug!("new cmd_reg={:#x}", command_reg);
            pci_config_write(0, DEVICE, 0, 0x04, command_reg);
            debug!(
                "cmd_reg={:#x} write finish",
                pci_config_read(0, DEVICE, 0, 0x04) // 0x100002，符合预期。高位的0x10是Status::CAPABILITIES_LIST
            );

            // 处理能力链表
            {
                debug!("Enter CAPABILITIES_LIST handle");
                let cap_ptr = (pci_config_read(0, DEVICE, 0, 0x34) & 0xFF) as u8; // 获取Capabilities指针
                debug!("cap_ptr={:#x}", cap_ptr);
                let mut offset = cap_ptr;
                while offset != 0 {
                    let cap_id = pci_config_read(0, DEVICE, 0, offset) & 0xFF;
                    let next_ptr = (pci_config_read(0, DEVICE, 0, offset + 1) & 0xFF) as u8;

                    if cap_id == 0x09 {
                        // VirtIO PCI Capability
                        let cfg_type = pci_config_read(0, DEVICE, 0, offset + 2) & 0xFF;
                        let bar_index = pci_config_read(0, DEVICE, 0, offset + 3) & 0xFF;
                        debug!(
                            "Found VirtIO Capability in BAR{}, cfg_type={}",
                            bar_index, cfg_type
                        );
                    }

                    offset = next_ptr;
                }
                debug!("Leave CAPABILITIES_LIST handle");
            }

            let bar0 = root.bar_info(func, 0).unwrap();
            match bar0 {
                BarInfo::Memory {
                    address_type: _,
                    prefetchable: _,
                    address: _,
                    size: _,
                } => {
                    //
                    panic!("We can assume bar0 is a IO bar!")
                }
                BarInfo::IO { address, size } => {
                    debug!("bar0 :(addr={}, size={})", address, size);
                }
            }
            // root.
            // 遍历6个bar，其中bar0是IO, bar1是mmio(32), bar4是mmio(64)
            while i < 6 {
                let bar_info = root.bar_info(func, i);
                let Ok(bar_info) = bar_info else {
                    panic!("bar {} is err.", i);
                    continue;
                };
                debug!("bar {} is ok.", i);
                if bar_info.takes_two_entries() {
                    debug!("bar {} takes_two_entries!", i);
                }

                if let BarInfo::Memory {
                    address_type,
                    size,
                    prefetchable,
                    address,
                } = bar_info
                {
                    debug!(
                        "bar: {:?}, {}, {}, {}",
                        address_type, size, prefetchable, address
                    );

                    match address_type {
                        MemoryBarType::Width32 => {
                            if size > 0 {
                                let addr = 0x40000000;
                                warn!("Allocated address: {:#x}", addr);
                                root.set_bar_32(func, i as u8, addr as u32);
                                //device.ranges.push(addr..addr + size);
                            }
                        }
                        MemoryBarType::Width64 => {
                            if size > 0 {
                                let addr = 0x40008000;
                                warn!("Allocated address: {:#x}", addr);
                                root.set_bar_64(func, i as u8, addr as u64);
                            }
                        }
                        _ => {
                            debug!("Memory BAR address type {:?} not supported.", address_type);
                        }
                    }
                } else if let BarInfo::IO { address, size } = bar_info {
                    debug!("IO bar: addr={:#x}, size={:#x}", address, size);
                }
                if bar_info.takes_two_entries() {
                    i += 2;
                } else {
                    i += 1;
                }
            }
            root.set_command(
                func,
                Command::IO_SPACE | Command::MEMORY_SPACE | Command::BUS_MASTER,
            );

            let mut transport = PciTransport::new::<H>(&mut root, func).unwrap();
            transport.set_status(DeviceStatus::empty());
            transport.set_status(DeviceStatus::ACKNOWLEDGE);

            log::debug!(
                "Detected virtio PCI device with device type {:?}, features {:#018x}, qs={}",
                transport.device_type(),
                transport.read_device_features(),
                transport.max_queue_size(0)
            );
            let ret = Self {
                inner: Mutex::new(
                    VirtIOBlk::<H, PciTransport>::new(transport).expect("VirtIOBlk create failed"),
                ),
            };
            ret
        }
    }
}

impl<H: Hal> BaseDriver for VirtIoBlkDev2<H> {
    fn device_name(&self) -> &str {
        "virtio-blk-pci"
    }

    fn device_type(&self) -> DeviceType {
        DeviceType::Block
    }
}

impl<H: Hal> BlockDriver for VirtIoBlkDev2<H> {
    #[inline]
    fn num_blocks(&self) -> usize {
        self.inner.lock().capacity() as usize
    }

    #[inline]
    fn block_size(&self) -> usize {
        512
    }

    fn read_block(&mut self, block_id: usize, buf: &mut [u8]) -> DevResult {
        self.inner
            .lock()
            .read_blocks(block_id as _, buf)
            .map_err(as_dev_err)
    }

    fn write_block(&mut self, block_id: usize, buf: &[u8]) -> DevResult {
        self.inner
            .lock()
            .write_blocks(block_id as _, buf)
            .map_err(as_dev_err)
    }

    fn flush(&mut self) -> DevResult {
        Ok(())
    }
}
