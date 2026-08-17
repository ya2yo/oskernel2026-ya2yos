//! In-memory anonymous regular file used for `O_TMPFILE`.
//!
//! Linux `O_TMPFILE` opens a regular file that is not linked into any
//! directory yet. The directory path passed to `openat` only selects the
//! filesystem/context; the returned fd owns the file until it is closed or
//! later materialized with `linkat("/proc/self/fd/<fd>", ...)`. This object
//! therefore stores data, offset, mode and owner in memory, and intentionally
//! does not create an ext4 directory entry.
use alloc::{sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use spin::Mutex;

use super::super::{File, Kstat, StMode};
use crate::mm::UserBuffer;
use crate::syscall::PollEvents;
use crate::utils::{SysErrNo, SysResult, SyscallRet};

static NEXT_TMP_INO: AtomicUsize = AtomicUsize::new(0x7000_0000);

const F_SEAL_SEAL: u32 = linux_raw_sys::general::F_SEAL_SEAL;
const F_SEAL_SHRINK: u32 = linux_raw_sys::general::F_SEAL_SHRINK;
const F_SEAL_GROW: u32 = linux_raw_sys::general::F_SEAL_GROW;
const F_SEAL_WRITE: u32 = linux_raw_sys::general::F_SEAL_WRITE;
const F_SEAL_FUTURE_WRITE: u32 = linux_raw_sys::general::F_SEAL_FUTURE_WRITE;
const F_SEAL_EXEC: u32 = linux_raw_sys::general::F_SEAL_EXEC;
const WRITE_SEALS: u32 = F_SEAL_WRITE | F_SEAL_FUTURE_WRITE;
const SUPPORTED_SEALS: u32 =
    F_SEAL_SEAL | F_SEAL_SHRINK | F_SEAL_GROW | F_SEAL_WRITE | F_SEAL_FUTURE_WRITE | F_SEAL_EXEC;

/// Anonymous regular file returned by `openat(..., O_TMPFILE, ...)`.
///
/// It deliberately implements only fd-visible state. A real filesystem inode is
/// created later only if userspace links `/proc/self/fd/<fd>` into a directory.
pub struct TmpFile {
    readable: bool,
    writable: bool,
    mode: u32,
    uid: u32,
    gid: u32,
    ino: usize,
    append: AtomicBool,
    is_memfd: bool,
    allow_sealing: bool,
    seals: AtomicU32,
    inner: Mutex<TmpFileInner>,
}

struct TmpFileInner {
    data: Vec<u8>,
    offset: usize,
}

impl TmpFile {
    pub fn new(readable: bool, writable: bool, mode: u32, uid: u32, gid: u32) -> Arc<Self> {
        Self::new_with_seals(readable, writable, mode, uid, gid, false, false, false)
    }

    /// Build the anonymous file used by `memfd_create(2)`.
    pub fn new_memfd(
        readable: bool,
        writable: bool,
        mode: u32,
        uid: u32,
        gid: u32,
        allow_sealing: bool,
        noexec_seal: bool,
    ) -> Arc<Self> {
        Self::new_with_seals(
            readable,
            writable,
            mode,
            uid,
            gid,
            allow_sealing,
            noexec_seal,
            true,
        )
    }

    fn new_with_seals(
        readable: bool,
        writable: bool,
        mode: u32,
        uid: u32,
        gid: u32,
        allow_sealing: bool,
        noexec_seal: bool,
        is_memfd: bool,
    ) -> Arc<Self> {
        let initial_seals = (if allow_sealing { 0 } else { F_SEAL_SEAL })
            | (if noexec_seal { F_SEAL_EXEC } else { 0 });
        Arc::new(Self {
            readable,
            writable,
            mode,
            uid,
            gid,
            ino: NEXT_TMP_INO.fetch_add(1, Ordering::Relaxed),
            append: AtomicBool::new(false),
            is_memfd,
            allow_sealing,
            seals: AtomicU32::new(initial_seals),
            inner: Mutex::new(TmpFileInner {
                data: Vec::new(),
                offset: 0,
            }),
        })
    }

    fn seals(&self) -> u32 {
        self.seals.load(Ordering::Acquire)
    }
}

impl File for TmpFile {
    fn readable(&self) -> bool {
        self.readable
    }

    fn writable(&self) -> bool {
        self.writable
    }

    fn read(&self, mut buf: UserBuffer) -> SyscallRet {
        let mut inner = self.inner.lock();
        if inner.offset >= inner.data.len() {
            return Ok(0);
        }
        let end = (inner.offset + buf.len()).min(inner.data.len());
        let read_len = buf.write(&inner.data[inner.offset..end]);
        inner.offset += read_len;
        Ok(read_len)
    }

    fn write(&self, buf: UserBuffer) -> SyscallRet {
        let bytes = buf.read_to_vec();
        let mut inner = self.inner.lock();
        let seals = self.seals();
        if seals & WRITE_SEALS != 0 {
            return Err(SysErrNo::EPERM);
        }
        if self.append.load(Ordering::Acquire) {
            inner.offset = inner.data.len();
        }
        let end = inner
            .offset
            .checked_add(bytes.len())
            .ok_or(SysErrNo::EFBIG)?;
        if end > inner.data.len() && seals & F_SEAL_GROW != 0 {
            return Err(SysErrNo::EPERM);
        }
        if end > inner.data.len() {
            inner.data.resize(end, 0);
        }
        let offset = inner.offset;
        inner.data[offset..end].copy_from_slice(&bytes);
        inner.offset = end;
        Ok(bytes.len())
    }

    fn truncate(&self, size: usize) -> SyscallRet {
        let mut inner = self.inner.lock();
        let seals = self.seals();
        if seals & WRITE_SEALS != 0
            || (size < inner.data.len() && seals & F_SEAL_SHRINK != 0)
            || (size > inner.data.len() && seals & F_SEAL_GROW != 0)
        {
            return Err(SysErrNo::EPERM);
        }
        inner.data.resize(size, 0);
        if inner.offset > size {
            inner.offset = size;
        }
        Ok(0)
    }

    fn fstat(&self) -> Kstat {
        let inner = self.inner.lock();
        Kstat {
            st_ino: self.ino,
            st_mode: StMode::FREG.bits() | (self.mode & 0o7777),
            // Anonymous tmpfiles have no directory entry until linkat()
            // materializes them, so Linux reports link count 0 here.
            st_nlink: 0,
            st_uid: self.uid,
            st_gid: self.gid,
            st_size: inner.data.len() as isize,
            st_blksize: 512,
            st_blocks: ((inner.data.len() + 511) / 512) as isize,
            ..Kstat::default()
        }
    }

    fn lseek(&self, offset: isize, whence: usize) -> SyscallRet {
        let mut inner = self.inner.lock();
        let base = match whence {
            0 => 0isize,
            1 => inner.offset as isize,
            2 => inner.data.len() as isize,
            _ => return Err(SysErrNo::EINVAL),
        };
        let new_offset = base.checked_add(offset).ok_or(SysErrNo::EINVAL)?;
        if new_offset < 0 {
            return Err(SysErrNo::EINVAL);
        }
        inner.offset = new_offset as usize;
        Ok(inner.offset)
    }

    fn poll(&self, events: PollEvents) -> PollEvents {
        let mut revents = PollEvents::empty();
        if events.contains(PollEvents::IN) && self.readable {
            revents |= PollEvents::IN;
        }
        if events.contains(PollEvents::OUT) && self.writable {
            revents |= PollEvents::OUT;
        }
        revents
    }

    fn set_append(&self, append: bool) -> SysResult {
        self.append.store(append, Ordering::Release);
        Ok(())
    }

    fn get_seals(&self) -> Result<u32, SysErrNo> {
        if self.is_memfd {
            Ok(self.seals())
        } else {
            Err(SysErrNo::EINVAL)
        }
    }

    fn add_seals(&self, seals: u32) -> SysResult {
        if !self.is_memfd || seals & !SUPPORTED_SEALS != 0 {
            return Err(SysErrNo::EINVAL);
        }
        if !self.allow_sealing {
            return Err(SysErrNo::EPERM);
        }

        let mut current = self.seals();
        loop {
            if current & F_SEAL_SEAL != 0 {
                return Err(SysErrNo::EPERM);
            }
            let updated = current | seals;
            match self.seals.compare_exchange_weak(
                current,
                updated,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Ok(()),
                Err(observed) => current = observed,
            }
        }
    }
}

trait UserBufferExt {
    fn read_to_vec(&self) -> Vec<u8>;
}

impl UserBufferExt for UserBuffer {
    /// Snapshot user buffer slices before taking the file lock, so writes can
    /// resize the backing Vec without keeping references into UserBuffer.
    fn read_to_vec(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        for slice in self.buffers.iter() {
            bytes.extend_from_slice(slice);
        }
        bytes
    }
}
