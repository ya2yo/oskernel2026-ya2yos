//! Polling AHCI/SATA block driver for the Loongson 2K1000 onboard controller.
//!
//! Linux describes the controller as `ahci@400e0000`. The controller accepts
//! only 32-bit DMA addresses, so all command metadata and the transfer buffer
//! are static kernel-image storage below physical 4GiB. `Disk` serializes this
//! synchronous driver, allowing a single command slot and shared DMA buffer.

use core::{
    cell::UnsafeCell,
    ptr,
    sync::atomic::{compiler_fence, Ordering},
};

use log::{info, warn};

use crate::{
    arch::memory_layout::KERNEL_ADDR_OFFSET,
    drivers::{BaseDriver, BlockDriver, DevError, DevResult, DeviceType},
};

const AHCI_BASE: usize = 0x400e_0000;
const HBA_CAP: usize = 0x00;
const HBA_GHC: usize = 0x04;
const HBA_PI: usize = 0x0c;
const HBA_GHC_HR: u32 = 1;
const HBA_GHC_AE: u32 = 1 << 31;
const HBA_CAP_SSS: u32 = 1 << 27;

const PORT_BASE: usize = 0x100;
const PORT_STRIDE: usize = 0x80;
const PX_CLB: usize = 0x00;
const PX_CLBU: usize = 0x04;
const PX_FB: usize = 0x08;
const PX_FBU: usize = 0x0c;
const PX_IS: usize = 0x10;
const PX_IE: usize = 0x14;
const PX_CMD: usize = 0x18;
const PX_TFD: usize = 0x20;
const PX_SSTS: usize = 0x28;
const PX_SCTL: usize = 0x2c;
const PX_SERR: usize = 0x30;
const PX_CI: usize = 0x38;
const PX_CMD_ST: u32 = 1;
const PX_CMD_SUD: u32 = 1 << 1;
const PX_CMD_FRE: u32 = 1 << 4;
const PX_CMD_FR: u32 = 1 << 14;
const PX_CMD_CR: u32 = 1 << 15;
const PX_IS_TFES: u32 = 1 << 30;

const ATA_STATUS_ERR: u32 = 1;
const ATA_STATUS_DF: u32 = 1 << 5;
const ATA_STATUS_BSY: u32 = 1 << 7;
const ATA_STATUS_DRQ: u32 = 1 << 3;
const ATA_CMD_IDENTIFY: u8 = 0xec;
const ATA_CMD_READ_DMA: u8 = 0xc8;
const ATA_CMD_WRITE_DMA: u8 = 0xca;
const ATA_CMD_READ_DMA_EXT: u8 = 0x25;
const ATA_CMD_WRITE_DMA_EXT: u8 = 0x35;
const ATA_CMD_FLUSH_CACHE: u8 = 0xe7;
const ATA_CMD_FLUSH_CACHE_EXT: u8 = 0xea;

const BLOCK_SIZE: usize = 512;
const COMMAND_SLOT: usize = 0;
const COMMAND_TIMEOUT_SPINS: usize = 20_000_000;
const PORT_TIMEOUT_SPINS: usize = 1_000_000;
const COMMAND_LIST_OFFSET: usize = 0;
const RECEIVED_FIS_OFFSET: usize = 1024;
const COMMAND_TABLE_OFFSET: usize = 1280;
const DATA_BUFFER_OFFSET: usize = 2048;
const DMA_MEMORY_SIZE: usize = DATA_BUFFER_OFFSET + BLOCK_SIZE;

#[repr(align(4096))]
struct AhciDmaMemory {
    bytes: UnsafeCell<[u8; DMA_MEMORY_SIZE]>,
}

unsafe impl Sync for AhciDmaMemory {}

impl AhciDmaMemory {
    const fn new() -> Self {
        Self {
            bytes: UnsafeCell::new([0; DMA_MEMORY_SIZE]),
        }
    }

    unsafe fn ptr_at(&self, offset: usize) -> *mut u8 {
        debug_assert!(offset < DMA_MEMORY_SIZE);
        (*self.bytes.get()).as_mut_ptr().add(offset)
    }

    unsafe fn command_list(&self) -> *mut u8 {
        self.ptr_at(COMMAND_LIST_OFFSET)
    }

    unsafe fn received_fis(&self) -> *mut u8 {
        self.ptr_at(RECEIVED_FIS_OFFSET)
    }

    unsafe fn command_table(&self) -> *mut u8 {
        self.ptr_at(COMMAND_TABLE_OFFSET)
    }

    unsafe fn data_buffer(&self) -> *mut u8 {
        self.ptr_at(DATA_BUFFER_OFFSET)
    }
}

static AHCI_DMA_MEMORY: AhciDmaMemory = AhciDmaMemory::new();

#[repr(C)]
struct CommandHeader {
    options: u16,
    prdt_length: u16,
    prdbc: u32,
    command_table_base: u32,
    command_table_base_upper: u32,
    reserved: [u32; 4],
}

/// A one-port polling-mode AHCI disk.
pub struct AhciBlkDev {
    hba: *mut u8,
    port: usize,
    num_blocks: usize,
    lba48: bool,
}

unsafe impl Send for AhciBlkDev {}
unsafe impl Sync for AhciBlkDev {}

impl AhciBlkDev {
    /// Keeps the architecture-selected block-device construction API uniform.
    pub fn new_device() -> Self {
        Self::new()
    }

    /// Finds and initializes the first attached SATA disk.
    pub fn new() -> Self {
        let hba = direct_map(AHCI_BASE);
        unsafe {
            Self::reset_hba(hba);
            let cap = read32(hba, HBA_CAP);
            if cap & HBA_CAP_SSS == 0 {
                write32(hba, HBA_CAP, cap | HBA_CAP_SSS);
            }
            // Some LS2K firmware revisions leave PI clear although port 0 is
            // wired and usable. Preserve a reported map when firmware has one.
            let reported_ports = read32(hba, HBA_PI);
            let ports = if reported_ports == 0 {
                warn!("ahci: HBA PI is clear; applying 2K1000 port-0 fallback");
                write32(hba, HBA_PI, 1);
                1
            } else {
                reported_ports
            };
            info!(
                "ahci: cap={:#010x}, implemented ports={:#010x}",
                read32(hba, HBA_CAP),
                ports
            );
            for port in 0..32 {
                if ports & (1 << port) == 0 {
                    continue;
                }
                let port_base = hba.add(PORT_BASE + port * PORT_STRIDE);
                if Self::prepare_port(port_base, port) {
                    return Self::identify(hba, port, port_base);
                }
            }
        }
        panic!("ahci: no SATA disk found on 2K1000 controller");
    }

    unsafe fn reset_hba(hba: *mut u8) {
        write32(hba, HBA_GHC, read32(hba, HBA_GHC) | HBA_GHC_HR);
        if !wait_until(PORT_TIMEOUT_SPINS, || {
            read32(hba, HBA_GHC) & HBA_GHC_HR == 0
        }) {
            warn!("ahci: HBA reset did not report completion; continuing");
        }
        write32(hba, HBA_GHC, read32(hba, HBA_GHC) | HBA_GHC_AE);
        if !wait_until(PORT_TIMEOUT_SPINS, || {
            read32(hba, HBA_GHC) & HBA_GHC_AE != 0
        }) {
            panic!("ahci: cannot enable AHCI mode");
        }
    }

    unsafe fn prepare_port(port_base: *mut u8, port: usize) -> bool {
        let mut cmd = read32(port_base, PX_CMD) & !(PX_CMD_ST | PX_CMD_FRE);
        write32(port_base, PX_CMD, cmd);
        if !wait_until(PORT_TIMEOUT_SPINS, || {
            read32(port_base, PX_CMD) & (PX_CMD_FR | PX_CMD_CR) == 0
        }) {
            warn!("ahci: port {port} did not stop command engine");
            return false;
        }
        write32(port_base, PX_IE, 0);
        write32(port_base, PX_SERR, u32::MAX);
        write32(port_base, PX_IS, u32::MAX);

        let sctl = read32(port_base, PX_SCTL);
        write32(port_base, PX_SCTL, (sctl & !0xf) | 1);
        for _ in 0..10_000 {
            core::hint::spin_loop();
        }
        write32(port_base, PX_SCTL, sctl & !0xf);
        if !wait_until(PORT_TIMEOUT_SPINS, || read32(port_base, PX_SSTS) & 0xf == 3) {
            info!(
                "ahci: port {port} no link (SSTS={:#010x})",
                read32(port_base, PX_SSTS)
            );
            return false;
        }

        let clb = AHCI_DMA_MEMORY.command_list();
        let fb = AHCI_DMA_MEMORY.received_fis();
        ptr::write_bytes(clb, 0, 1024);
        ptr::write_bytes(fb, 0, 256);
        let clb_pa = physical_address(clb);
        let fb_pa = physical_address(fb);
        write32(port_base, PX_CLB, clb_pa as u32);
        write32(port_base, PX_CLBU, (clb_pa >> 32) as u32);
        write32(port_base, PX_FB, fb_pa as u32);
        write32(port_base, PX_FBU, (fb_pa >> 32) as u32);

        cmd = read32(port_base, PX_CMD) | PX_CMD_FRE | PX_CMD_SUD;
        write32(port_base, PX_CMD, cmd | PX_CMD_ST);
        true
    }

    unsafe fn identify(hba: *mut u8, port: usize, port_base: *mut u8) -> Self {
        let data = AHCI_DMA_MEMORY.data_buffer();
        ptr::write_bytes(data, 0, BLOCK_SIZE);
        Self::issue(
            port_base,
            ATA_CMD_IDENTIFY,
            false,
            0,
            false,
            0,
            data,
            BLOCK_SIZE,
        )
        .expect("ahci: IDENTIFY DEVICE failed");
        let lba48_supported = read_le_u16(data, 83 * 2) & (1 << 10) != 0;
        let lba28 = read_le_u32(data, 60 * 2) as u64;
        let lba48 = read_le_u64(data, 100 * 2);
        let capacity = if lba48_supported && lba48 != 0 {
            lba48
        } else {
            lba28
        };
        if capacity == 0 || capacity > usize::MAX as u64 {
            panic!("ahci: invalid disk capacity {capacity:#x}");
        }
        info!(
            "ahci: port {port}: {capacity} sectors, LBA48={}",
            lba48_supported && lba48 != 0
        );
        Self {
            hba,
            port,
            num_blocks: capacity as usize,
            lba48: lba48_supported && lba48 != 0,
        }
    }

    #[allow(clippy::too_many_arguments)]
    unsafe fn issue(
        port_base: *mut u8,
        command: u8,
        write: bool,
        lba: usize,
        lba48: bool,
        sector_count: u16,
        data: *mut u8,
        data_len: usize,
    ) -> DevResult {
        debug_assert!(data_len == 0 || data_len == BLOCK_SIZE);
        if !wait_until(PORT_TIMEOUT_SPINS, || {
            read32(port_base, PX_TFD) & (ATA_STATUS_BSY | ATA_STATUS_DRQ) == 0
        }) {
            warn!("ahci: ATA command {command:#x} while port busy");
            return Err(DevError::Io);
        }
        write32(port_base, PX_SERR, u32::MAX);
        write32(port_base, PX_IS, u32::MAX);

        let command_table = AHCI_DMA_MEMORY.command_table();
        ptr::write_bytes(command_table, 0, 512);
        let cfis = command_table;
        cfis.add(0).write_volatile(0x27);
        cfis.add(1).write_volatile(1 << 7);
        cfis.add(2).write_volatile(command);
        cfis.add(4).write_volatile(lba as u8);
        cfis.add(5).write_volatile((lba >> 8) as u8);
        cfis.add(6).write_volatile((lba >> 16) as u8);
        cfis.add(7)
            .write_volatile(0xe0 | ((lba >> 24) as u8 & 0x0f));
        if lba48 {
            cfis.add(8).write_volatile((lba >> 24) as u8);
            cfis.add(9).write_volatile((lba >> 32) as u8);
            cfis.add(10).write_volatile((lba >> 40) as u8);
        }
        cfis.add(12).write_volatile(sector_count as u8);
        cfis.add(13).write_volatile((sector_count >> 8) as u8);

        let header = (AHCI_DMA_MEMORY.command_list() as *mut CommandHeader).add(COMMAND_SLOT);
        let prdt_length = u16::from(data_len != 0);
        if data_len != 0 {
            let data_pa = physical_address(data);
            let prdt = command_table.add(128) as *mut u32;
            prdt.add(0).write_volatile(data_pa as u32);
            prdt.add(1).write_volatile((data_pa >> 32) as u32);
            prdt.add(2).write_volatile(0);
            prdt.add(3).write_volatile((data_len - 1) as u32);
        }
        let table_pa = physical_address(command_table);
        ptr::write_volatile(
            header,
            CommandHeader {
                options: 5 | ((write as u16) << 6),
                prdt_length,
                prdbc: 0,
                command_table_base: table_pa as u32,
                command_table_base_upper: (table_pa >> 32) as u32,
                reserved: [0; 4],
            },
        );

        compiler_fence(Ordering::Release);
        write32(port_base, PX_CI, 1 << COMMAND_SLOT);
        if !wait_until(COMMAND_TIMEOUT_SPINS, || {
            read32(port_base, PX_CI) & (1 << COMMAND_SLOT) == 0
        }) {
            warn!("ahci: ATA command {command:#x} timed out");
            return Err(DevError::Io);
        }
        compiler_fence(Ordering::Acquire);
        let task_file = read32(port_base, PX_TFD);
        let is = read32(port_base, PX_IS);
        if task_file & (ATA_STATUS_ERR | ATA_STATUS_DF) != 0 || is & PX_IS_TFES != 0 {
            warn!("ahci: ATA command {command:#x} failed: TFD={task_file:#010x}, IS={is:#010x}");
            write32(port_base, PX_IS, is);
            return Err(DevError::Io);
        }
        Ok(())
    }

    fn port_base(&self) -> *mut u8 {
        unsafe { self.hba.add(PORT_BASE + self.port * PORT_STRIDE) }
    }

    fn transfer(&mut self, block_id: usize, buffer: *mut u8, len: usize, write: bool) -> DevResult {
        if len == 0 {
            return Ok(());
        }
        if len % BLOCK_SIZE != 0 {
            return Err(DevError::InvalidParam);
        }
        let count = len / BLOCK_SIZE;
        let Some(end) = block_id.checked_add(count) else {
            return Err(DevError::InvalidParam);
        };
        if end > self.num_blocks {
            return Err(DevError::InvalidParam);
        }
        for index in 0..count {
            let lba = block_id + index;
            let data = unsafe { AHCI_DMA_MEMORY.data_buffer() };
            unsafe {
                if write {
                    ptr::copy_nonoverlapping(buffer.add(index * BLOCK_SIZE), data, BLOCK_SIZE);
                }
                let lba48 = self.lba48 && lba > 0x0fff_ffff;
                Self::issue(
                    self.port_base(),
                    match (write, lba48) {
                        (false, false) => ATA_CMD_READ_DMA,
                        (true, false) => ATA_CMD_WRITE_DMA,
                        (false, true) => ATA_CMD_READ_DMA_EXT,
                        (true, true) => ATA_CMD_WRITE_DMA_EXT,
                    },
                    write,
                    lba,
                    lba48,
                    1,
                    data,
                    BLOCK_SIZE,
                )?;
                if !write {
                    ptr::copy_nonoverlapping(data, buffer.add(index * BLOCK_SIZE), BLOCK_SIZE);
                }
            }
        }
        Ok(())
    }
}

impl BaseDriver for AhciBlkDev {
    fn device_name(&self) -> &str {
        "ls2k1000-ahci"
    }
    fn device_type(&self) -> DeviceType {
        DeviceType::Block
    }
}

impl BlockDriver for AhciBlkDev {
    fn num_blocks(&self) -> usize {
        self.num_blocks
    }
    fn block_size(&self) -> usize {
        BLOCK_SIZE
    }
    fn read_block(&mut self, block_id: usize, buffer: &mut [u8]) -> DevResult {
        self.transfer(block_id, buffer.as_mut_ptr(), buffer.len(), false)
    }
    fn write_block(&mut self, block_id: usize, buffer: &[u8]) -> DevResult {
        self.transfer(block_id, buffer.as_ptr() as *mut u8, buffer.len(), true)
    }
    fn flush(&mut self) -> DevResult {
        unsafe {
            Self::issue(
                self.port_base(),
                if self.lba48 {
                    ATA_CMD_FLUSH_CACHE_EXT
                } else {
                    ATA_CMD_FLUSH_CACHE
                },
                false,
                0,
                self.lba48,
                0,
                AHCI_DMA_MEMORY.data_buffer(),
                0,
            )
        }
    }
}

#[inline]
fn direct_map(physical_address: usize) -> *mut u8 {
    (physical_address + KERNEL_ADDR_OFFSET) as *mut u8
}

#[inline]
fn physical_address(kernel_address: *mut u8) -> u64 {
    let address = kernel_address as usize;
    assert!(
        address >= KERNEL_ADDR_OFFSET,
        "AHCI DMA buffer is not direct mapped"
    );
    let physical = (address - KERNEL_ADDR_OFFSET) as u64;
    assert!(
        physical <= u32::MAX as u64,
        "AHCI DMA address exceeds 32 bits"
    );
    physical
}

#[inline]
unsafe fn wait_until(mut spins: usize, condition: impl Fn() -> bool) -> bool {
    while spins != 0 {
        if condition() {
            return true;
        }
        spins -= 1;
        core::hint::spin_loop();
    }
    false
}

#[inline]
unsafe fn read32(base: *mut u8, offset: usize) -> u32 {
    (base.add(offset) as *const u32).read_volatile()
}
#[inline]
unsafe fn write32(base: *mut u8, offset: usize, value: u32) {
    (base.add(offset) as *mut u32).write_volatile(value)
}
#[inline]
unsafe fn read_le_u16(base: *const u8, offset: usize) -> u16 {
    u16::from_le_bytes([
        base.add(offset).read_volatile(),
        base.add(offset + 1).read_volatile(),
    ])
}
#[inline]
unsafe fn read_le_u32(base: *const u8, offset: usize) -> u32 {
    u32::from_le_bytes([
        base.add(offset).read_volatile(),
        base.add(offset + 1).read_volatile(),
        base.add(offset + 2).read_volatile(),
        base.add(offset + 3).read_volatile(),
    ])
}
#[inline]
unsafe fn read_le_u64(base: *const u8, offset: usize) -> u64 {
    u64::from_le_bytes([
        base.add(offset).read_volatile(),
        base.add(offset + 1).read_volatile(),
        base.add(offset + 2).read_volatile(),
        base.add(offset + 3).read_volatile(),
        base.add(offset + 4).read_volatile(),
        base.add(offset + 5).read_volatile(),
        base.add(offset + 6).read_volatile(),
        base.add(offset + 7).read_volatile(),
    ])
}
