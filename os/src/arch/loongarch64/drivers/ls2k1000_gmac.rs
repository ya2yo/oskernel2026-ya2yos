//! Polling driver for the first Loongson 2K1000 DWMAC 3.70a controller.
//!
//! The LS2K1000-DP firmware FDT exposes GMAC0 at `0x4004_0000`, with a
//! Clause-22 PHY at address zero. Linux's `dwmac-loongson` selects the
//! DWMAC1000/3.70 register and normal descriptor layout for this controller.
//! Ya2yOS deliberately uses one DMA channel in polling mode until the LIOINTC
//! and NAPI paths are available.

use alloc::{boxed::Box, sync::Arc, vec::Vec};
use core::{
    ptr::{read_volatile, write_volatile, NonNull},
    sync::atomic::{compiler_fence, Ordering},
};

use crate::{
    arch::memory_layout::{KERNEL_ADDR_OFFSET, PAGE_SIZE},
    drivers::{
        BaseDriver, DevError, DevResult, DeviceType, EthernetAddress, NetBuf, NetBufPool,
        NetBufPtr, NetDriverOps,
    },
    mm::cma_alloc,
};

const GMAC0_PA: usize = 0x4004_0000;
const RING_SIZE: usize = 16;
const DMA_BUF_SIZE: usize = 2047;
const NET_BUF_SIZE: usize = 2047;
const MDIO_TIMEOUT: usize = 100_000;
const DMA_RESET_TIMEOUT: usize = 200_000_000;

const GMAC_CONTROL: usize = 0x0000;
const GMAC_FRAME_FILTER: usize = 0x0004;
const GMAC_MII_ADDR: usize = 0x0010;
const GMAC_MII_DATA: usize = 0x0014;
const GMAC_INT_MASK: usize = 0x003c;
const GMAC_ADDR0_HIGH: usize = 0x0040;
const GMAC_ADDR0_LOW: usize = 0x0044;
const GMAC_RGSMIIIS: usize = 0x00d8;

const DMA_BUS_MODE: usize = 0x1000;
const DMA_XMT_POLL_DEMAND: usize = 0x1004;
const DMA_RCV_POLL_DEMAND: usize = 0x1008;
const DMA_RCV_BASE_ADDR: usize = 0x100c;
const DMA_TX_BASE_ADDR: usize = 0x1010;
const DMA_STATUS: usize = 0x1014;
const DMA_CONTROL: usize = 0x1018;
const DMA_INTR_ENA: usize = 0x101c;

const GMAC_CONTROL_JD: u32 = 1 << 22;
const GMAC_CONTROL_BE: u32 = 1 << 21;
const GMAC_CONTROL_DCRS: u32 = 1 << 16;
const GMAC_CONTROL_PS: u32 = 1 << 15;
const GMAC_CONTROL_FES: u32 = 1 << 14;
const GMAC_CONTROL_DM: u32 = 1 << 11;
const GMAC_CONTROL_TE: u32 = 1 << 3;
const GMAC_CONTROL_RE: u32 = 1 << 2;
const GMAC_FRAME_FILTER_PM: u32 = 1 << 4;
const GMAC_ADDR_AE: u32 = 1 << 31;
const GMAC_RGSMIIIS_SPEED_MASK: u32 = 0b11 << 1;
const GMAC_RGSMIIIS_SPEED_125: u32 = 0b10 << 1;
const GMAC_RGSMIIIS_SPEED_25: u32 = 0b01 << 1;
const GMAC_RGSMIIIS_LINK: u32 = 1 << 3;

const DMA_BUS_MODE_SWR: u32 = 1;
const DMA_BUS_MODE_PBL_32: u32 = 32 << 8;
const DMA_BUS_MODE_RPBL_32: u32 = 32 << 17;
const DMA_BUS_MODE_USP: u32 = 1 << 23;
const DMA_BUS_MODE_MAXPBL: u32 = 1 << 24;
const DMA_CONTROL_RSF: u32 = 1 << 25;
const DMA_CONTROL_TSF: u32 = 1 << 21;
const DMA_CONTROL_ST: u32 = 1 << 13;
const DMA_CONTROL_SR: u32 = 1 << 1;

const MII_BUSY: u32 = 1;
const MII_CLK_100_150_MHZ: u32 = 1 << 2;
const MII_PHY_SHIFT: u32 = 11;
const MII_REG_SHIFT: u32 = 6;
const MII_PHYSID1: u8 = 2;
const MII_PHYSID2: u8 = 3;

const DESC_OWN: u32 = 1 << 31;
const RX_DESC_FIRST: u32 = 1 << 9;
const RX_DESC_LAST: u32 = 1 << 8;
const RX_DESC_ERROR: u32 = 1 << 15;
const RX_DESC_FRAME_LEN: u32 = 0x3fff << 16;
const DESC_BUF1_SIZE: u32 = 0x7ff;
const DESC_END_RING: u32 = 1 << 25;
const TX_DESC_FIRST: u32 = 1 << 29;
const TX_DESC_LAST: u32 = 1 << 30;
const TX_DESC_INTERRUPT: u32 = 1 << 31;

#[repr(C, align(16))]
#[derive(Clone, Copy, Default)]
struct DmaDesc {
    des0: u32,
    des1: u32,
    des2: u32,
    des3: u32,
}

struct DmaMemory {
    paddr: usize,
    vaddr: NonNull<u8>,
}

impl DmaMemory {
    fn allocate(pages: usize) -> DevResult<Self> {
        let paddr = cma_alloc(pages).ok_or(DevError::NoMemory)?.0;
        // The v3 normal descriptor format stores a 32-bit buffer address.
        if paddr > u32::MAX as usize {
            return Err(DevError::NoMemory);
        }
        let vaddr =
            NonNull::new((paddr + KERNEL_ADDR_OFFSET) as *mut u8).ok_or(DevError::NoMemory)?;
        unsafe { core::ptr::write_bytes(vaddr.as_ptr(), 0, pages * PAGE_SIZE) };
        Ok(Self { paddr, vaddr })
    }
}

/// Single-channel polling GMAC0 driver for the 2K1000-DP reference board.
pub struct Ls2k1000Gmac {
    mac: usize,
    phy_addr: u8,
    mac_addr: EthernetAddress,
    tx_desc: DmaMemory,
    rx_desc: DmaMemory,
    tx_buffers: Vec<DmaMemory>,
    rx_buffers: Vec<DmaMemory>,
    tx_next: usize,
    tx_clean: usize,
    rx_next: usize,
    tx_in_flight: [bool; RING_SIZE],
    buf_pool: Arc<NetBufPool>,
}

unsafe impl Send for Ls2k1000Gmac {}
unsafe impl Sync for Ls2k1000Gmac {}

impl Ls2k1000Gmac {
    pub fn try_new_device() -> Option<Self> {
        match Self::try_new() {
            Ok(device) => Some(device),
            Err(error) => {
                log::warn!("2K1000 GMAC0 initialization failed: {:?}", error);
                None
            }
        }
    }

    fn try_new() -> DevResult<Self> {
        let tx_desc = DmaMemory::allocate(1)?;
        let rx_desc = DmaMemory::allocate(1)?;
        let mut tx_buffers = Vec::with_capacity(RING_SIZE);
        let mut rx_buffers = Vec::with_capacity(RING_SIZE);
        for _ in 0..RING_SIZE {
            tx_buffers.push(DmaMemory::allocate(1)?);
            rx_buffers.push(DmaMemory::allocate(1)?);
        }

        let mut driver = Self {
            mac: GMAC0_PA + KERNEL_ADDR_OFFSET,
            phy_addr: 0,
            mac_addr: EthernetAddress([0; 6]),
            tx_desc,
            rx_desc,
            tx_buffers,
            rx_buffers,
            tx_next: 0,
            tx_clean: 0,
            rx_next: 0,
            tx_in_flight: [false; RING_SIZE],
            buf_pool: NetBufPool::new(RING_SIZE * 2, NET_BUF_SIZE)?,
        };

        driver.reset_dma()?;
        driver.phy_addr = driver.find_phy()?;
        driver.mac_addr = driver.read_or_create_mac();
        driver.program_mac_address();
        driver.configure_mac();
        driver.configure_rings();
        driver.refresh_link();
        driver.start();
        log::info!("2K1000 GMAC0 initialized with PHY {}", driver.phy_addr);
        Ok(driver)
    }

    fn reset_dma(&self) -> DevResult {
        self.write(DMA_BUS_MODE, self.read(DMA_BUS_MODE) | DMA_BUS_MODE_SWR);
        for _ in 0..DMA_RESET_TIMEOUT {
            if self.read(DMA_BUS_MODE) & DMA_BUS_MODE_SWR == 0 {
                return Ok(());
            }
            core::hint::spin_loop();
        }
        Err(DevError::Io)
    }

    fn configure_mac(&self) {
        let control = GMAC_CONTROL_JD | GMAC_CONTROL_BE | GMAC_CONTROL_DCRS | GMAC_CONTROL_DM;
        self.write(GMAC_CONTROL, control);
        // Polling does not use either the MAC or DMA interrupt lines.
        self.write(GMAC_INT_MASK, u32::MAX);
        self.write(DMA_INTR_ENA, 0);
        self.write(GMAC_FRAME_FILTER, GMAC_FRAME_FILTER_PM);
    }

    fn configure_rings(&mut self) {
        self.write(
            DMA_BUS_MODE,
            DMA_BUS_MODE_PBL_32 | DMA_BUS_MODE_RPBL_32 | DMA_BUS_MODE_USP | DMA_BUS_MODE_MAXPBL,
        );
        self.write(DMA_TX_BASE_ADDR, self.tx_desc.paddr as u32);
        self.write(DMA_RCV_BASE_ADDR, self.rx_desc.paddr as u32);
        for index in 0..RING_SIZE {
            self.write_tx_desc(
                index,
                DmaDesc {
                    des1: Self::ring_bit(index),
                    ..DmaDesc::default()
                },
            );
            self.rearm_rx_desc(index);
        }
        Self::dma_fence();
        self.write(DMA_STATUS, u32::MAX);
        self.write(
            DMA_CONTROL,
            DMA_CONTROL_RSF | DMA_CONTROL_TSF | DMA_CONTROL_ST | DMA_CONTROL_SR,
        );
    }

    fn start(&self) {
        self.write(
            GMAC_CONTROL,
            self.read(GMAC_CONTROL) | GMAC_CONTROL_TE | GMAC_CONTROL_RE,
        );
        self.write(DMA_RCV_POLL_DEMAND, 0);
    }

    fn find_phy(&self) -> DevResult<u8> {
        // The board FDT specifies PHY address zero. Probe it first, then
        // retain a scan fallback for compatible 2K1000 board revisions.
        for phy in core::iter::once(0).chain(1..32) {
            let id1 = self.mdio_read(phy, MII_PHYSID1)?;
            let id2 = self.mdio_read(phy, MII_PHYSID2)?;
            let id = ((id1 as u32) << 16) | id2 as u32;
            if id != 0 && id != u32::MAX && id != 0xffff_ffff {
                return Ok(phy);
            }
        }
        Err(DevError::Io)
    }

    fn refresh_link(&self) {
        let status = self.read(GMAC_RGSMIIIS);
        if status & GMAC_RGSMIIIS_LINK == 0 {
            return;
        }
        let mut control = self.read(GMAC_CONTROL);
        control &= !(GMAC_CONTROL_PS | GMAC_CONTROL_FES | GMAC_CONTROL_DM);
        control |= GMAC_CONTROL_DM;
        match status & GMAC_RGSMIIIS_SPEED_MASK {
            GMAC_RGSMIIIS_SPEED_125 => {}
            GMAC_RGSMIIIS_SPEED_25 => control |= GMAC_CONTROL_PS | GMAC_CONTROL_FES,
            _ => control |= GMAC_CONTROL_PS,
        }
        self.write(GMAC_CONTROL, control);
    }

    fn read_or_create_mac(&self) -> EthernetAddress {
        let low = self.read(GMAC_ADDR0_LOW);
        let high = self.read(GMAC_ADDR0_HIGH);
        let address = [
            low as u8,
            (low >> 8) as u8,
            (low >> 16) as u8,
            (low >> 24) as u8,
            high as u8,
            (high >> 8) as u8,
        ];
        if address != [0; 6] && address != [0xff; 6] {
            EthernetAddress(address)
        } else {
            EthernetAddress([0x02, 0x56, 0x46, 0x32, 0x00, 0x01])
        }
    }

    fn program_mac_address(&self) {
        let address = self.mac_addr.0;
        self.write(
            GMAC_ADDR0_HIGH,
            GMAC_ADDR_AE | ((address[5] as u32) << 8) | address[4] as u32,
        );
        self.write(
            GMAC_ADDR0_LOW,
            (address[0] as u32)
                | ((address[1] as u32) << 8)
                | ((address[2] as u32) << 16)
                | ((address[3] as u32) << 24),
        );
    }

    fn mdio_read(&self, phy: u8, register: u8) -> DevResult<u16> {
        self.wait_mdio()?;
        self.write(
            GMAC_MII_ADDR,
            ((phy as u32) << MII_PHY_SHIFT)
                | ((register as u32) << MII_REG_SHIFT)
                | MII_CLK_100_150_MHZ
                | MII_BUSY,
        );
        self.wait_mdio()?;
        Ok(self.read(GMAC_MII_DATA) as u16)
    }

    fn wait_mdio(&self) -> DevResult {
        for _ in 0..MDIO_TIMEOUT {
            if self.read(GMAC_MII_ADDR) & MII_BUSY == 0 {
                return Ok(());
            }
            core::hint::spin_loop();
        }
        Err(DevError::Io)
    }

    fn rearm_rx_desc(&self, index: usize) {
        let descriptor = DmaDesc {
            des0: DESC_OWN,
            des1: (DMA_BUF_SIZE as u32 & DESC_BUF1_SIZE) | Self::ring_bit(index),
            des2: self.rx_buffers[index].paddr as u32,
            des3: 0,
        };
        self.write_rx_desc(index, descriptor);
        Self::dma_fence();
    }

    fn reclaim_tx(&mut self) {
        while self.tx_in_flight[self.tx_clean] {
            if self.read_tx_desc(self.tx_clean).des0 & DESC_OWN != 0 {
                break;
            }
            self.write_tx_desc(
                self.tx_clean,
                DmaDesc {
                    des1: Self::ring_bit(self.tx_clean),
                    ..DmaDesc::default()
                },
            );
            self.tx_in_flight[self.tx_clean] = false;
            self.tx_clean = (self.tx_clean + 1) % RING_SIZE;
        }
    }

    #[inline]
    fn ring_bit(index: usize) -> u32 {
        if index + 1 == RING_SIZE {
            DESC_END_RING
        } else {
            0
        }
    }

    #[inline]
    fn read_tx_desc(&self, index: usize) -> DmaDesc {
        unsafe { read_volatile(self.tx_desc.vaddr.as_ptr().cast::<DmaDesc>().add(index)) }
    }

    #[inline]
    fn write_tx_desc(&self, index: usize, descriptor: DmaDesc) {
        unsafe {
            write_volatile(
                self.tx_desc.vaddr.as_ptr().cast::<DmaDesc>().add(index),
                descriptor,
            )
        }
    }

    #[inline]
    fn read_rx_desc(&self, index: usize) -> DmaDesc {
        unsafe { read_volatile(self.rx_desc.vaddr.as_ptr().cast::<DmaDesc>().add(index)) }
    }

    #[inline]
    fn write_rx_desc(&self, index: usize, descriptor: DmaDesc) {
        unsafe {
            write_volatile(
                self.rx_desc.vaddr.as_ptr().cast::<DmaDesc>().add(index),
                descriptor,
            )
        }
    }

    #[inline]
    fn dma_fence() {
        // The LoongArch direct-map window is uncached; retain compiler ordering
        // between descriptor stores and the volatile DMA doorbell write.
        compiler_fence(Ordering::SeqCst);
    }

    #[inline]
    fn read(&self, offset: usize) -> u32 {
        unsafe { read_volatile((self.mac + offset) as *const u32) }
    }

    #[inline]
    fn write(&self, offset: usize, value: u32) {
        unsafe { write_volatile((self.mac + offset) as *mut u32, value) }
    }
}

impl BaseDriver for Ls2k1000Gmac {
    fn device_name(&self) -> &str {
        "loongson-2k1000-gmac0"
    }

    fn device_type(&self) -> DeviceType {
        DeviceType::Net
    }
}

impl NetDriverOps for Ls2k1000Gmac {
    fn mac_address(&self) -> EthernetAddress {
        EthernetAddress(self.mac_addr.0)
    }

    fn can_transmit(&self) -> bool {
        !self.tx_in_flight[self.tx_next] && self.read_tx_desc(self.tx_next).des0 & DESC_OWN == 0
    }

    fn can_receive(&self) -> bool {
        self.read_rx_desc(self.rx_next).des0 & DESC_OWN == 0
    }

    fn rx_queue_size(&self) -> usize {
        RING_SIZE
    }

    fn tx_queue_size(&self) -> usize {
        RING_SIZE
    }

    fn recycle_rx_buffer(&mut self, rx_buf: NetBufPtr) -> DevResult {
        drop(unsafe { NetBuf::from_buf_ptr(rx_buf) });
        Ok(())
    }

    fn recycle_tx_buffers(&mut self) -> DevResult {
        self.reclaim_tx();
        Ok(())
    }

    fn transmit(&mut self, tx_buf: NetBufPtr) -> DevResult {
        self.reclaim_tx();
        if !self.can_transmit() {
            drop(unsafe { NetBuf::from_buf_ptr(tx_buf) });
            return Err(DevError::Again);
        }
        let packet = tx_buf.packet();
        if packet.is_empty() || packet.len() > DMA_BUF_SIZE {
            drop(unsafe { NetBuf::from_buf_ptr(tx_buf) });
            return Err(DevError::InvalidParam);
        }
        let index = self.tx_next;
        unsafe {
            core::ptr::copy_nonoverlapping(
                packet.as_ptr(),
                self.tx_buffers[index].vaddr.as_ptr(),
                packet.len(),
            );
        }
        let length = packet.len() as u32;
        drop(unsafe { NetBuf::from_buf_ptr(tx_buf) });
        self.write_tx_desc(
            index,
            DmaDesc {
                des0: DESC_OWN,
                des1: (length & DESC_BUF1_SIZE)
                    | Self::ring_bit(index)
                    | TX_DESC_FIRST
                    | TX_DESC_LAST
                    | TX_DESC_INTERRUPT,
                des2: self.tx_buffers[index].paddr as u32,
                des3: 0,
            },
        );
        Self::dma_fence();
        self.write(DMA_XMT_POLL_DEMAND, 0);
        self.tx_in_flight[index] = true;
        self.tx_next = (index + 1) % RING_SIZE;
        self.refresh_link();
        Ok(())
    }

    fn receive(&mut self) -> DevResult<NetBufPtr> {
        let index = self.rx_next;
        let descriptor = self.read_rx_desc(index);
        if descriptor.des0 & DESC_OWN != 0 {
            return Err(DevError::Again);
        }
        let frame_len = ((descriptor.des0 & RX_DESC_FRAME_LEN) >> 16) as usize;
        let valid = descriptor.des0 & (RX_DESC_FIRST | RX_DESC_LAST | RX_DESC_ERROR)
            == RX_DESC_FIRST | RX_DESC_LAST;
        if !valid || frame_len <= 4 || frame_len > DMA_BUF_SIZE {
            self.rearm_rx_desc(index);
            self.write(DMA_RCV_POLL_DEMAND, 0);
            self.rx_next = (index + 1) % RING_SIZE;
            return Err(DevError::Io);
        }
        // Linux's DWMAC receive path removes the FCS reported in RDES0.
        let packet_len = frame_len - 4;
        let mut rx_buf = Box::new(self.buf_pool.alloc().ok_or(DevError::Again)?);
        rx_buf.set_header_len(0);
        rx_buf.set_packet_len(packet_len);
        unsafe {
            core::ptr::copy_nonoverlapping(
                self.rx_buffers[index].vaddr.as_ptr(),
                rx_buf.packet_mut().as_mut_ptr(),
                packet_len,
            );
        }
        self.rearm_rx_desc(index);
        self.write(DMA_RCV_POLL_DEMAND, 0);
        self.rx_next = (index + 1) % RING_SIZE;
        self.refresh_link();
        Ok(rx_buf.into_buf_ptr())
    }

    fn alloc_tx_buffer(&mut self, size: usize) -> DevResult<NetBufPtr> {
        if size == 0 || size > DMA_BUF_SIZE {
            return Err(DevError::InvalidParam);
        }
        let mut tx_buf = Box::new(self.buf_pool.alloc().ok_or(DevError::Again)?);
        tx_buf.set_header_len(0);
        tx_buf.set_packet_len(size);
        Ok(tx_buf.into_buf_ptr())
    }
}
