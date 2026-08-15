//! StarFive JH7110 GMAC1 driver used by VisionFive 2.
//!
//! The controller is a Synopsys DWMAC 5.20 compatible EQOS instance.  This is
//! deliberately a polling driver: Ya2yOS does not yet have the IRQ/NAPI and
//! cache-maintenance infrastructure used by Linux `stmmac`.  DMA descriptors
//! and controller-owned buffers are therefore allocated from CMA, while
//! packets are copied into/out of the existing `NetBufPool` at the driver
//! boundary.

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

const GMAC1_PA: usize = 0x1604_0000;
const SYS_CRG_PA: usize = 0x1302_0000;
const SYS_SYSCON_PA: usize = 0x1303_0000;

const RING_SIZE: usize = 16;
const DMA_BUF_SIZE: usize = 2048;
const NET_BUF_SIZE: usize = 2048;
const MDIO_TIMEOUT: usize = 100_000;
const DMA_RESET_TIMEOUT: usize = 1_000_000;

const MAC_CONFIGURATION: usize = 0x0000;
const MAC_PACKET_FILTER: usize = 0x0008;
const MAC_RX_FLOW_CTRL: usize = 0x0090;
const MAC_TXQ_PRTY_MAP0: usize = 0x0098;
const MAC_RXQ_CTRL0: usize = 0x00a0;
const MAC_RXQ_CTRL2: usize = 0x00a8;
const MAC_MDIO_ADDR: usize = 0x0200;
const MAC_MDIO_DATA: usize = 0x0204;
const MAC_ADDR0_HIGH: usize = 0x0300;
const MAC_ADDR0_LOW: usize = 0x0304;

const MTL_TXQ0_OP_MODE: usize = 0x0d00;
const MTL_TXQ0_QUANTUM_WEIGHT: usize = 0x0d18;
const MTL_RXQ0_OP_MODE: usize = 0x0d30;

const DMA_MODE: usize = 0x1000;
const DMA_SYS_BUS_MODE: usize = 0x1004;
const DMA_CH0_CONTROL: usize = 0x1100;
const DMA_CH0_TX_CONTROL: usize = 0x1104;
const DMA_CH0_RX_CONTROL: usize = 0x1108;
const DMA_CH0_TX_DESC_LIST_HI: usize = 0x1110;
const DMA_CH0_TX_DESC_LIST: usize = 0x1114;
const DMA_CH0_RX_DESC_LIST_HI: usize = 0x1118;
const DMA_CH0_RX_DESC_LIST: usize = 0x111c;
const DMA_CH0_TX_DESC_TAIL: usize = 0x1120;
const DMA_CH0_RX_DESC_TAIL: usize = 0x1128;
const DMA_CH0_TX_RING_LEN: usize = 0x112c;
const DMA_CH0_RX_RING_LEN: usize = 0x1130;

const GMAC_CONFIG_PS: u32 = 1 << 15;
const GMAC_CONFIG_FES: u32 = 1 << 14;
const GMAC_CONFIG_DM: u32 = 1 << 13;
const GMAC_CONFIG_TE: u32 = 1 << 1;
const GMAC_CONFIG_RE: u32 = 1;
const GMAC_PACKET_FILTER_PM: u32 = 1 << 4;
const GMAC_RXQ0_ENABLE_DCB: u32 = 2;
const GMAC_TX_FLOW_CTRL_TFE: u32 = 1 << 1;
const GMAC_RX_FLOW_CTRL_RFE: u32 = 1;

const MTL_TXQ_TSF: u32 = 1 << 1;
const MTL_TXQ_ENABLE: u32 = 2 << 2;
const MTL_RXQ_RSF: u32 = 1 << 5;
const MTL_RXQ_FEP: u32 = 1 << 4;
const MTL_RXQ_FUP: u32 = 1 << 3;

const DMA_MODE_SWR: u32 = 1;
const DMA_SYS_BUS_BLEN: u32 = (1 << 1) | (1 << 2) | (1 << 3);
const DMA_CH_PBLX8: u32 = 1 << 16;
const DMA_TX_PBL: u32 = 16 << 16;
const DMA_TX_OSP: u32 = 1 << 4;
const DMA_TX_START: u32 = 1;
const DMA_RX_BUF_SIZE: u32 = (DMA_BUF_SIZE as u32) << 1;
const DMA_RX_PBL: u32 = 8 << 16;
const DMA_RX_START: u32 = 1;

const DESC_OWN: u32 = 1 << 31;
const DESC_IOC: u32 = 1 << 30;
const TX_DESC_FIRST: u32 = 1 << 29;
const DESC_LAST: u32 = 1 << 28;
const RX_DESC_BUF1_VALID: u32 = 1 << 24;
const DESC_FRAME_LEN: u32 = 0x7fff;
const RX_DESC_ERROR: u32 = 1 << 15;

const MDIO_BUSY: u32 = 1;
const MDIO_READ: u32 = 3 << 2;
const MDIO_WRITE: u32 = 1 << 2;
const MDIO_CR_500_800MHZ: u32 = 7 << 8;

const MII_BMSR: u8 = 1;
const MII_PHYSID1: u8 = 2;
const MII_PHYSID2: u8 = 3;
const BMSR_LINK: u16 = 1 << 2;
const YTPHY_STATUS: u8 = 0x11;
const YTPHY_DUPLEX: u16 = 1 << 13;
const YTPHY_SPEED_MASK: u16 = 0b11 << 14;

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
        let vaddr =
            NonNull::new((paddr + KERNEL_ADDR_OFFSET) as *mut u8).ok_or(DevError::NoMemory)?;
        unsafe { core::ptr::write_bytes(vaddr.as_ptr(), 0, pages * PAGE_SIZE) };
        Ok(Self { paddr, vaddr })
    }
}

/// Polling DWMAC driver for the VF2's onboard GMAC1/YT8531 combination.
pub struct VisionFive2Gmac {
    mac: usize,
    dma: usize,
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

unsafe impl Send for VisionFive2Gmac {}
unsafe impl Sync for VisionFive2Gmac {}

impl VisionFive2Gmac {
    /// Initializes the clocks, MAC, PHY link settings, and DMA rings.
    pub fn try_new_device() -> Option<Self> {
        match Self::try_new() {
            Ok(driver) => Some(driver),
            Err(err) => {
                log::warn!("VisionFive2 GMAC1 initialization failed: {:?}", err);
                None
            }
        }
    }

    fn try_new() -> DevResult<Self> {
        Self::enable_clocks_and_select_rgmii();

        let tx_desc = DmaMemory::allocate(1)?;
        let rx_desc = DmaMemory::allocate(1)?;
        let mut tx_buffers = Vec::with_capacity(RING_SIZE);
        let mut rx_buffers = Vec::with_capacity(RING_SIZE);
        for _ in 0..RING_SIZE {
            tx_buffers.push(DmaMemory::allocate(1)?);
            rx_buffers.push(DmaMemory::allocate(1)?);
        }

        let mut driver = Self {
            mac: GMAC1_PA + KERNEL_ADDR_OFFSET,
            dma: GMAC1_PA + KERNEL_ADDR_OFFSET,
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
        driver.mac_addr = driver.read_or_create_mac();
        driver.program_mac_address();
        driver.phy_addr = driver.find_phy()?;
        driver.configure_link();
        driver.configure_mac_and_mtl();
        driver.configure_dma_rings();
        driver.start();
        log::info!("VisionFive2 GMAC1 initialized with PHY {}", driver.phy_addr);
        Ok(driver)
    }

    fn enable_clocks_and_select_rgmii() {
        let crg = SYS_CRG_PA + KERNEL_ADDR_OFFSET;
        for offset in [0x184, 0x188, 0x198, 0x1a4, 0x1ac] {
            Self::set_bits_at(crg, offset, 1 << 31);
        }
        Self::clear_bits_at(crg, 0x300, (1 << 2) | (1 << 3));

        // Linux's `starfive,syscon = <&sys_syscon 0x90 0x2>` selects
        // PHY_INTF_SEL_RGMII (value 1) in the three-bit field at bit 2.
        let syscon = SYS_SYSCON_PA + KERNEL_ADDR_OFFSET;
        let value = Self::read_at(syscon, 0x90);
        Self::write_at(syscon, 0x90, (value & !(0x7 << 2)) | (1 << 2));
    }

    fn reset_dma(&self) -> DevResult {
        self.write_dma(DMA_MODE, DMA_MODE_SWR);
        for _ in 0..DMA_RESET_TIMEOUT {
            if self.read_dma(DMA_MODE) & DMA_MODE_SWR == 0 {
                return Ok(());
            }
            core::hint::spin_loop();
        }
        Err(DevError::Io)
    }

    fn configure_mac_and_mtl(&self) {
        self.modify_mac(MAC_RXQ_CTRL0, 0x3, GMAC_RXQ0_ENABLE_DCB);
        self.modify_mac(MAC_TXQ_PRTY_MAP0, 0xff, 0);
        self.modify_mac(MAC_RXQ_CTRL2, 0xff, 0);
        self.set_mac_bits(MAC_RX_FLOW_CTRL, GMAC_RX_FLOW_CTRL_RFE);
        self.set_mac_bits(MAC_PACKET_FILTER, GMAC_PACKET_FILTER_PM);
        self.set_mac_bits(0x70, (0xffff << 16) | GMAC_TX_FLOW_CTRL_TFE);

        self.set_mac_bits(MTL_TXQ0_OP_MODE, MTL_TXQ_TSF | MTL_TXQ_ENABLE);
        self.write_mac(MTL_TXQ0_QUANTUM_WEIGHT, 0x10);
        self.set_mac_bits(MTL_RXQ0_OP_MODE, MTL_RXQ_RSF | MTL_RXQ_FEP | MTL_RXQ_FUP);
    }

    fn configure_dma_rings(&mut self) {
        self.set_dma_bits(DMA_SYS_BUS_MODE, DMA_SYS_BUS_BLEN);
        self.set_dma_bits(DMA_CH0_CONTROL, DMA_CH_PBLX8);
        self.set_dma_bits(DMA_CH0_TX_CONTROL, DMA_TX_PBL | DMA_TX_OSP);
        self.set_dma_bits(DMA_CH0_RX_CONTROL, DMA_RX_BUF_SIZE | DMA_RX_PBL);

        self.write_dma(DMA_CH0_TX_DESC_LIST_HI, 0);
        self.write_dma(DMA_CH0_TX_DESC_LIST, self.tx_desc.paddr as u32);
        self.write_dma(DMA_CH0_RX_DESC_LIST_HI, 0);
        self.write_dma(DMA_CH0_RX_DESC_LIST, self.rx_desc.paddr as u32);
        // DWMAC 5.20 encodes N descriptors as N - 1; Linux's
        // `stmmac_set_rings_length()` programs the same representation.
        self.write_dma(DMA_CH0_TX_RING_LEN, (RING_SIZE - 1) as u32);
        self.write_dma(DMA_CH0_RX_RING_LEN, (RING_SIZE - 1) as u32);

        for index in 0..RING_SIZE {
            self.write_tx_desc(index, DmaDesc::default());
            self.rearm_rx_desc(index);
        }
        self.write_dma(
            DMA_CH0_RX_DESC_TAIL,
            self.rx_desc_addr(RING_SIZE - 1) as u32,
        );
    }

    fn start(&self) {
        self.set_dma_bits(DMA_CH0_TX_CONTROL, DMA_TX_START);
        self.set_dma_bits(DMA_CH0_RX_CONTROL, DMA_RX_START);
        self.set_mac_bits(MAC_CONFIGURATION, GMAC_CONFIG_TE | GMAC_CONFIG_RE);
    }

    fn find_phy(&self) -> DevResult<u8> {
        for phy in 0..32 {
            let id1 = self.mdio_read(phy, MII_PHYSID1)?;
            let id2 = self.mdio_read(phy, MII_PHYSID2)?;
            let id = ((id1 as u32) << 16) | id2 as u32;
            if id != 0 && id != u32::MAX && id != 0xffff_ffff {
                if id != 0x4f51_e91b {
                    log::warn!("VisionFive2 GMAC1 found unrecognized PHY id {:#010x}", id);
                }
                return Ok(phy);
            }
        }
        Err(DevError::Io)
    }

    fn configure_link(&self) {
        let _ = self.mdio_read(self.phy_addr, MII_BMSR); // BMSR link is latch-low.
        let Ok(status) = self.mdio_read(self.phy_addr, YTPHY_STATUS) else {
            return;
        };
        let mut config = self.read_mac(MAC_CONFIGURATION);
        config &= !(GMAC_CONFIG_PS | GMAC_CONFIG_FES | GMAC_CONFIG_DM);
        match (status & YTPHY_SPEED_MASK) >> 14 {
            0 => config |= GMAC_CONFIG_PS,
            1 => config |= GMAC_CONFIG_PS | GMAC_CONFIG_FES,
            _ => {}
        }
        if status & YTPHY_DUPLEX != 0 {
            config |= GMAC_CONFIG_DM;
        }
        self.write_mac(MAC_CONFIGURATION, config);
    }

    fn refresh_link(&self) {
        if let Ok(status) = self.mdio_read(self.phy_addr, MII_BMSR) {
            if status & BMSR_LINK != 0 {
                self.configure_link();
            }
        }
    }

    fn read_or_create_mac(&self) -> EthernetAddress {
        let low = self.read_mac(MAC_ADDR0_LOW);
        let high = self.read_mac(MAC_ADDR0_HIGH);
        let addr = [
            low as u8,
            (low >> 8) as u8,
            (low >> 16) as u8,
            (low >> 24) as u8,
            high as u8,
            (high >> 8) as u8,
        ];
        if addr != [0; 6] && addr != [0xff; 6] {
            EthernetAddress(addr)
        } else {
            // A locally administered fallback keeps the interface usable if
            // firmware has not supplied an OTP-derived address.
            EthernetAddress([0x02, 0x56, 0x46, 0x32, 0x00, 0x01])
        }
    }

    fn program_mac_address(&self) {
        let addr = self.mac_addr.0;
        self.write_mac(
            MAC_ADDR0_HIGH,
            ((addr[5] as u32) << 8) | addr[4] as u32 | (1 << 31),
        );
        self.write_mac(
            MAC_ADDR0_LOW,
            (addr[0] as u32)
                | ((addr[1] as u32) << 8)
                | ((addr[2] as u32) << 16)
                | ((addr[3] as u32) << 24),
        );
    }

    fn rearm_rx_desc(&self, index: usize) {
        let desc = DmaDesc {
            des0: self.rx_buffers[index].paddr as u32,
            des1: 0,
            des2: 0,
            des3: DESC_OWN | DESC_IOC | RX_DESC_BUF1_VALID,
        };
        self.write_rx_desc(index, desc);
        Self::dma_fence();
    }

    fn reclaim_tx(&mut self) {
        while self.tx_in_flight[self.tx_clean] {
            let desc = self.read_tx_desc(self.tx_clean);
            if desc.des3 & DESC_OWN != 0 {
                break;
            }
            self.write_tx_desc(self.tx_clean, DmaDesc::default());
            self.tx_in_flight[self.tx_clean] = false;
            self.tx_clean = (self.tx_clean + 1) % RING_SIZE;
        }
    }

    #[inline]
    fn tx_desc_addr(&self, index: usize) -> usize {
        self.tx_desc.paddr + index * core::mem::size_of::<DmaDesc>()
    }

    #[inline]
    fn rx_desc_addr(&self, index: usize) -> usize {
        self.rx_desc.paddr + index * core::mem::size_of::<DmaDesc>()
    }

    #[inline]
    fn read_tx_desc(&self, index: usize) -> DmaDesc {
        unsafe { read_volatile(self.tx_desc.vaddr.as_ptr().cast::<DmaDesc>().add(index)) }
    }

    #[inline]
    fn write_tx_desc(&self, index: usize, desc: DmaDesc) {
        unsafe {
            write_volatile(
                self.tx_desc.vaddr.as_ptr().cast::<DmaDesc>().add(index),
                desc,
            )
        }
    }

    #[inline]
    fn read_rx_desc(&self, index: usize) -> DmaDesc {
        unsafe { read_volatile(self.rx_desc.vaddr.as_ptr().cast::<DmaDesc>().add(index)) }
    }

    #[inline]
    fn write_rx_desc(&self, index: usize, desc: DmaDesc) {
        unsafe {
            write_volatile(
                self.rx_desc.vaddr.as_ptr().cast::<DmaDesc>().add(index),
                desc,
            )
        }
    }

    fn mdio_read(&self, phy: u8, reg: u8) -> DevResult<u16> {
        self.wait_mdio()?;
        self.write_mac(
            MAC_MDIO_ADDR,
            ((phy as u32) << 21)
                | ((reg as u32) << 16)
                | MDIO_CR_500_800MHZ
                | MDIO_READ
                | MDIO_BUSY,
        );
        self.wait_mdio()?;
        Ok(self.read_mac(MAC_MDIO_DATA) as u16)
    }

    fn wait_mdio(&self) -> DevResult {
        for _ in 0..MDIO_TIMEOUT {
            if self.read_mac(MAC_MDIO_ADDR) & MDIO_BUSY == 0 {
                return Ok(());
            }
            core::hint::spin_loop();
        }
        Err(DevError::Io)
    }

    #[inline]
    fn dma_fence() {
        compiler_fence(Ordering::Release);
        unsafe { core::arch::asm!("fence iorw, iorw", options(nostack, preserves_flags)) };
    }

    #[inline]
    fn read_at(base: usize, offset: usize) -> u32 {
        unsafe { read_volatile((base + offset) as *const u32) }
    }

    #[inline]
    fn write_at(base: usize, offset: usize, value: u32) {
        unsafe { write_volatile((base + offset) as *mut u32, value) }
    }

    #[inline]
    fn set_bits_at(base: usize, offset: usize, bits: u32) {
        Self::write_at(base, offset, Self::read_at(base, offset) | bits);
    }

    #[inline]
    fn clear_bits_at(base: usize, offset: usize, bits: u32) {
        Self::write_at(base, offset, Self::read_at(base, offset) & !bits);
    }

    #[inline]
    fn read_mac(&self, offset: usize) -> u32 {
        Self::read_at(self.mac, offset)
    }
    #[inline]
    fn write_mac(&self, offset: usize, value: u32) {
        Self::write_at(self.mac, offset, value)
    }
    #[inline]
    fn set_mac_bits(&self, offset: usize, bits: u32) {
        Self::set_bits_at(self.mac, offset, bits)
    }
    #[inline]
    fn modify_mac(&self, offset: usize, clear: u32, set: u32) {
        Self::write_at(self.mac, offset, (self.read_mac(offset) & !clear) | set)
    }
    #[inline]
    fn read_dma(&self, offset: usize) -> u32 {
        Self::read_at(self.dma, offset)
    }
    #[inline]
    fn write_dma(&self, offset: usize, value: u32) {
        Self::write_at(self.dma, offset, value)
    }
    #[inline]
    fn set_dma_bits(&self, offset: usize, bits: u32) {
        Self::set_bits_at(self.dma, offset, bits)
    }
}

impl BaseDriver for VisionFive2Gmac {
    fn device_name(&self) -> &str {
        "starfive-jh7110-gmac1"
    }
    fn device_type(&self) -> DeviceType {
        DeviceType::Net
    }
    fn irq_num(&self) -> Option<usize> {
        Some(78)
    }
}

impl NetDriverOps for VisionFive2Gmac {
    fn mac_address(&self) -> EthernetAddress {
        EthernetAddress(self.mac_addr.0)
    }

    fn can_transmit(&self) -> bool {
        !self.tx_in_flight[self.tx_next] && self.read_tx_desc(self.tx_next).des3 & DESC_OWN == 0
    }

    fn can_receive(&self) -> bool {
        self.read_rx_desc(self.rx_next).des3 & DESC_OWN == 0
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
        if packet.len() > DMA_BUF_SIZE {
            drop(unsafe { NetBuf::from_buf_ptr(tx_buf) });
            return Err(DevError::InvalidParam);
        }
        let packet_len = packet.len();
        unsafe {
            core::ptr::copy_nonoverlapping(
                packet.as_ptr(),
                self.tx_buffers[self.tx_next].vaddr.as_ptr(),
                packet_len,
            );
        }
        drop(unsafe { NetBuf::from_buf_ptr(tx_buf) });

        let index = self.tx_next;
        self.write_tx_desc(
            index,
            DmaDesc {
                des0: self.tx_buffers[index].paddr as u32,
                des1: 0,
                des2: packet_len as u32 | DESC_IOC,
                des3: DESC_OWN | TX_DESC_FIRST | DESC_LAST | packet_len as u32,
            },
        );
        Self::dma_fence();
        self.write_dma(DMA_CH0_TX_DESC_TAIL, self.tx_desc_addr(index) as u32);
        self.tx_in_flight[index] = true;
        self.tx_next = (index + 1) % RING_SIZE;
        self.refresh_link();
        Ok(())
    }

    fn receive(&mut self) -> DevResult<NetBufPtr> {
        let index = self.rx_next;
        let desc = self.read_rx_desc(index);
        if desc.des3 & DESC_OWN != 0 {
            return Err(DevError::Again);
        }

        let frame_len = (desc.des3 & DESC_FRAME_LEN) as usize;
        let valid = (desc.des3 & (RX_DESC_ERROR | TX_DESC_FIRST | DESC_LAST))
            == (TX_DESC_FIRST | DESC_LAST);
        if !valid || frame_len <= 4 || frame_len > DMA_BUF_SIZE {
            self.rearm_rx_desc(index);
            self.write_dma(DMA_CH0_RX_DESC_TAIL, self.rx_desc_addr(index) as u32);
            self.rx_next = (index + 1) % RING_SIZE;
            return Err(DevError::Io);
        }

        // DWMAC reports the FCS in RDES3's packet length; Linux's stmmac
        // receive path removes the same four bytes before handing up a frame.
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
        self.write_dma(DMA_CH0_RX_DESC_TAIL, self.rx_desc_addr(index) as u32);
        self.rx_next = (index + 1) % RING_SIZE;
        self.refresh_link();
        Ok(rx_buf.into_buf_ptr())
    }

    fn alloc_tx_buffer(&mut self, size: usize) -> DevResult<NetBufPtr> {
        if size > DMA_BUF_SIZE {
            return Err(DevError::InvalidParam);
        }
        let mut tx_buf = Box::new(self.buf_pool.alloc().ok_or(DevError::Again)?);
        tx_buf.set_header_len(0);
        tx_buf.set_packet_len(size);
        Ok(tx_buf.into_buf_ptr())
    }
}
