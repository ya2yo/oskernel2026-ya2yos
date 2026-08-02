use core::convert::TryFrom;

use lwext4_rust::KernelDevOp;
use spin::Mutex;
use virtio_drivers::{
    device::blk::{VirtIOBlk, SECTOR_SIZE},
    transport::Transport,
    Hal,
};

const BLOCK_SIZE: usize = 512;

/// A position-independent disk adapter used by the standalone example.
pub struct Disk<H: Hal, T: Transport> {
    dev: Mutex<VirtIOBlk<H, T>>,
    size: u64,
}

impl<H: Hal, T: Transport> Disk<H, T> {
    /// Create a new disk.
    pub fn new(dev: VirtIOBlk<H, T>) -> Self {
        assert_eq!(BLOCK_SIZE, SECTOR_SIZE);
        Self {
            size: dev.capacity() * BLOCK_SIZE as u64,
            dev: Mutex::new(dev),
        }
    }

    /// Get the size of the disk. `capacity()` is measured in 512-byte units.
    pub fn size(&self) -> u64 {
        self.size
    }

    fn check_range(&self, offset: u64, len: usize) -> Result<(), i32> {
        let len = u64::try_from(len).map_err(|_| -1)?;
        match offset.checked_add(len) {
            Some(end) if end <= self.size => Ok(()),
            _ => Err(-1),
        }
    }

    /// Read an exact byte range without changing shared request state.
    pub fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize, i32> {
        self.check_range(offset, buf.len())?;
        if buf.is_empty() {
            return Ok(0);
        }

        let mut dev = self.dev.lock();
        let mut block_id = usize::try_from(offset / BLOCK_SIZE as u64).map_err(|_| -1)?;
        let mut in_block = usize::try_from(offset % BLOCK_SIZE as u64).map_err(|_| -1)?;
        let mut done = 0;

        while done < buf.len() {
            let remaining = buf.len() - done;
            if in_block == 0 && remaining >= BLOCK_SIZE {
                let bulk_len = remaining / BLOCK_SIZE * BLOCK_SIZE;
                dev.read_blocks(block_id, &mut buf[done..done + bulk_len])
                    .map_err(as_disk_err)?;
                done += bulk_len;
                block_id += bulk_len / BLOCK_SIZE;
                continue;
            }

            let mut block = [0u8; BLOCK_SIZE];
            dev.read_blocks(block_id, &mut block).map_err(as_disk_err)?;
            let count = remaining.min(BLOCK_SIZE - in_block);
            buf[done..done + count].copy_from_slice(&block[in_block..in_block + count]);
            done += count;
            block_id += 1;
            in_block = 0;
        }

        Ok(done)
    }

    /// Write an exact byte range without changing shared request state.
    pub fn write_at(&self, offset: u64, buf: &[u8]) -> Result<usize, i32> {
        self.check_range(offset, buf.len())?;
        if buf.is_empty() {
            return Ok(0);
        }

        let mut dev = self.dev.lock();
        let mut block_id = usize::try_from(offset / BLOCK_SIZE as u64).map_err(|_| -1)?;
        let mut in_block = usize::try_from(offset % BLOCK_SIZE as u64).map_err(|_| -1)?;
        let mut done = 0;

        while done < buf.len() {
            let remaining = buf.len() - done;
            if in_block == 0 && remaining >= BLOCK_SIZE {
                let bulk_len = remaining / BLOCK_SIZE * BLOCK_SIZE;
                dev.write_blocks(block_id, &buf[done..done + bulk_len])
                    .map_err(as_disk_err)?;
                done += bulk_len;
                block_id += bulk_len / BLOCK_SIZE;
                continue;
            }

            let mut block = [0u8; BLOCK_SIZE];
            dev.read_blocks(block_id, &mut block).map_err(as_disk_err)?;
            let count = remaining.min(BLOCK_SIZE - in_block);
            block[in_block..in_block + count].copy_from_slice(&buf[done..done + count]);
            dev.write_blocks(block_id, &block).map_err(as_disk_err)?;
            done += count;
            block_id += 1;
            in_block = 0;
        }

        Ok(done)
    }

    pub fn flush(&self) -> Result<(), i32> {
        self.dev.lock().flush().map_err(as_disk_err)
    }
}

impl<H: Hal, T: Transport> KernelDevOp for Disk<H, T> {
    type DevType = Self;

    fn device_size(dev: &Self::DevType) -> Result<u64, i32> {
        Ok(dev.size())
    }

    fn read_at(dev: &Self::DevType, offset: u64, buf: &mut [u8]) -> Result<usize, i32> {
        dev.read_at(offset, buf)
    }

    fn write_at(dev: &Self::DevType, offset: u64, buf: &[u8]) -> Result<usize, i32> {
        dev.write_at(offset, buf)
    }

    fn flush(dev: &Self::DevType) -> Result<usize, i32> {
        dev.flush()?;
        Ok(0)
    }
}

const fn as_disk_err(e: virtio_drivers::Error) -> i32 {
    use virtio_drivers::Error::*;
    match e {
        QueueFull => -1,
        NotReady => -2,
        WrongToken => -3,
        AlreadyUsed => -4,
        InvalidParam => -5,
        DmaError => -6,
        IoError => -7,
        Unsupported => -8,
        ConfigSpaceTooSmall => -9,
        ConfigSpaceMissing => -10,
        _ => -127,
    }
}
