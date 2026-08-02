use crate::drivers::BlockDriver;
#[cfg(feature = "perf")]
use crate::{
    arch::time::get_ticks,
    utils::perf::{
        record_ext4_block_request_acquired, record_ext4_block_request_complete,
        record_ext4_block_request_submit, Ext4BlockRequestKind,
    },
};
use spin::Mutex;

use super::{BlockDeviceImpl, DevError, DevResult};

const BLOCK_SIZE: usize = 512;

/// A position-independent disk adapter.
///
/// The mutex is a device-submission boundary. It covers a complete request,
/// including an unaligned read-modify-write, but it never protects filesystem
/// metadata, block-cache state, or inode state.
pub struct Disk {
    dev: Mutex<BlockDeviceImpl>,
    size: usize,
}

impl Disk {
    /// Create a new disk.
    pub fn new(dev: BlockDeviceImpl) -> Self {
        assert_eq!(BLOCK_SIZE, dev.block_size());
        Self {
            size: dev.num_blocks() * BLOCK_SIZE,
            dev: Mutex::new(dev),
        }
    }

    /// Get the size of the disk.
    pub fn size(&self) -> usize {
        self.size
    }

    #[inline]
    fn check_range(&self, offset: usize, len: usize) -> DevResult {
        match offset.checked_add(len) {
            Some(end) if end <= self.size => Ok(()),
            _ => Err(DevError::InvalidParam),
        }
    }

    /// Read an exact byte range without changing shared request state.
    pub fn read_at(&self, offset: usize, buf: &mut [u8]) -> DevResult<usize> {
        self.check_range(offset, buf.len())?;
        if buf.is_empty() {
            return Ok(0);
        }

        #[cfg(feature = "perf")]
        let request_started = {
            record_ext4_block_request_submit(Ext4BlockRequestKind::Read, buf.len());
            get_ticks()
        };
        #[cfg(feature = "perf")]
        let (mut dev, contended) = match self.dev.try_lock() {
            Some(dev) => (dev, false),
            None => (self.dev.lock(), true),
        };
        #[cfg(feature = "perf")]
        let service_started = {
            let acquired = get_ticks();
            record_ext4_block_request_acquired(acquired.saturating_sub(request_started), contended);
            get_ticks()
        };
        #[cfg(not(feature = "perf"))]
        let mut dev = self.dev.lock();

        let result = (|| {
            let mut block_id = offset / BLOCK_SIZE;
            let mut in_block = offset % BLOCK_SIZE;
            let mut done = 0;

            while done < buf.len() {
                let remaining = buf.len() - done;
                if in_block == 0 && remaining >= BLOCK_SIZE {
                    let bulk_len = remaining / BLOCK_SIZE * BLOCK_SIZE;
                    dev.read_block(block_id, &mut buf[done..done + bulk_len])?;
                    done += bulk_len;
                    block_id += bulk_len / BLOCK_SIZE;
                    continue;
                }

                let mut block = [0u8; BLOCK_SIZE];
                dev.read_block(block_id, &mut block)?;
                let count = remaining.min(BLOCK_SIZE - in_block);
                buf[done..done + count].copy_from_slice(&block[in_block..in_block + count]);
                done += count;
                block_id += 1;
                in_block = 0;
            }

            Ok(done)
        })();
        #[cfg(feature = "perf")]
        record_ext4_block_request_complete(
            get_ticks().saturating_sub(service_started),
            result.is_ok(),
        );
        result
    }

    /// Write an exact byte range without changing shared request state.
    pub fn write_at(&self, offset: usize, buf: &[u8]) -> DevResult<usize> {
        self.check_range(offset, buf.len())?;
        if buf.is_empty() {
            return Ok(0);
        }

        #[cfg(feature = "perf")]
        let request_started = {
            record_ext4_block_request_submit(Ext4BlockRequestKind::Write, buf.len());
            get_ticks()
        };
        #[cfg(feature = "perf")]
        let (mut dev, contended) = match self.dev.try_lock() {
            Some(dev) => (dev, false),
            None => (self.dev.lock(), true),
        };
        #[cfg(feature = "perf")]
        let service_started = {
            let acquired = get_ticks();
            record_ext4_block_request_acquired(acquired.saturating_sub(request_started), contended);
            get_ticks()
        };
        #[cfg(not(feature = "perf"))]
        let mut dev = self.dev.lock();

        let result = (|| {
            let mut block_id = offset / BLOCK_SIZE;
            let mut in_block = offset % BLOCK_SIZE;
            let mut done = 0;

            while done < buf.len() {
                let remaining = buf.len() - done;
                if in_block == 0 && remaining >= BLOCK_SIZE {
                    let bulk_len = remaining / BLOCK_SIZE * BLOCK_SIZE;
                    dev.write_block(block_id, &buf[done..done + bulk_len])?;
                    done += bulk_len;
                    block_id += bulk_len / BLOCK_SIZE;
                    continue;
                }

                let mut block = [0u8; BLOCK_SIZE];
                dev.read_block(block_id, &mut block)?;
                let count = remaining.min(BLOCK_SIZE - in_block);
                block[in_block..in_block + count].copy_from_slice(&buf[done..done + count]);
                dev.write_block(block_id, &block)?;
                done += count;
                block_id += 1;
                in_block = 0;
            }

            Ok(done)
        })();
        #[cfg(feature = "perf")]
        record_ext4_block_request_complete(
            get_ticks().saturating_sub(service_started),
            result.is_ok(),
        );
        result
    }

    /// Complete writes already submitted to the device.
    pub fn flush(&self) -> DevResult {
        #[cfg(feature = "perf")]
        let request_started = {
            record_ext4_block_request_submit(Ext4BlockRequestKind::Flush, 0);
            get_ticks()
        };
        #[cfg(feature = "perf")]
        let (mut dev, contended) = match self.dev.try_lock() {
            Some(dev) => (dev, false),
            None => (self.dev.lock(), true),
        };
        #[cfg(feature = "perf")]
        let service_started = {
            let acquired = get_ticks();
            record_ext4_block_request_acquired(acquired.saturating_sub(request_started), contended);
            get_ticks()
        };
        #[cfg(not(feature = "perf"))]
        let mut dev = self.dev.lock();

        let result = dev.flush();
        #[cfg(feature = "perf")]
        record_ext4_block_request_complete(
            get_ticks().saturating_sub(service_started),
            result.is_ok(),
        );
        result
    }
}
