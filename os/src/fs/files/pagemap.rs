use crate::{
    arch::memory_layout::PAGE_SIZE,
    fs::{File, Kstat, StMode, SEEK_CUR, SEEK_END, SEEK_SET},
    mm::{MemorySet, UserBuffer, VirtPageNum},
    syscall::PollEvents,
    utils::{SysErrNo, SyscallRet},
};
use alloc::{borrow::Cow, string::String, sync::Arc, vec};
use spin::Mutex;

const PAGEMAP_ENTRY_SIZE: usize = core::mem::size_of::<u64>();
const PAGEMAP_PFN_MASK: u64 = (1u64 << 55) - 1;
const PAGEMAP_PRESENT: u64 = 1u64 << 63;

/// Read-only, on-demand view of `/proc/<pid>/pagemap`.
///
/// The ext4 backing store does not implement sparse `truncate()` efficiently:
/// enlarging a pagemap to the highest user VMA would allocate hundreds of MiB
/// for every forked process. Keep only the proc directory entry on ext4 and
/// synthesize pagemap entries when the descriptor is read.
pub struct PagemapFile {
    memory_set: Arc<MemorySet>,
    path: String,
    offset: Mutex<usize>,
}

impl PagemapFile {
    pub fn open(memory_set: Arc<MemorySet>, path: String) -> Arc<Self> {
        Arc::new(Self {
            memory_set,
            path,
            offset: Mutex::new(0),
        })
    }

    fn size(&self) -> Result<usize, SysErrNo> {
        let memory_set = self.memory_set.get_ref();
        memory_set
            .areas
            .iter()
            .map(|area| area.vpn_range.end().0)
            .max()
            .unwrap_or(0)
            .checked_mul(PAGEMAP_ENTRY_SIZE)
            .ok_or(SysErrNo::EFBIG)
    }

    fn fill_entries(&self, offset: usize, data: &mut [u8]) -> Result<(), SysErrNo> {
        let end = offset.checked_add(data.len()).ok_or(SysErrNo::EFBIG)?;
        let memory_set = self.memory_set.get_ref();
        let first_vpn = offset / PAGEMAP_ENTRY_SIZE;
        let last_vpn = end.saturating_sub(1) / PAGEMAP_ENTRY_SIZE;

        for vpn in first_vpn..=last_vpn {
            let entry_start = vpn.checked_mul(PAGEMAP_ENTRY_SIZE).ok_or(SysErrNo::EFBIG)?;
            let entry_end = entry_start + PAGEMAP_ENTRY_SIZE;
            let copy_start = offset.max(entry_start);
            let copy_end = end.min(entry_end);

            let entry = memory_set
                .areas
                .iter()
                .any(|area| area.vpn_range.start().0 <= vpn && vpn < area.vpn_range.end().0)
                .then(|| memory_set.page_table.translate(VirtPageNum::from(vpn)))
                .flatten()
                .map(|ppn| PAGEMAP_PRESENT | (ppn.0 as u64 & PAGEMAP_PFN_MASK))
                .unwrap_or(0);
            let entry_bytes = entry.to_ne_bytes();
            let src_start = copy_start - entry_start;
            let dst_start = copy_start - offset;
            data[dst_start..dst_start + copy_end - copy_start]
                .copy_from_slice(&entry_bytes[src_start..src_start + copy_end - copy_start]);
        }
        Ok(())
    }
}

impl File for PagemapFile {
    fn readable(&self) -> bool {
        true
    }

    fn writable(&self) -> bool {
        false
    }

    fn read(&self, mut buf: UserBuffer) -> SyscallRet {
        let mut offset = self.offset.lock();
        let size = self.size()?;
        if *offset >= size {
            return Ok(0);
        }

        let read_len = buf.len().min(size - *offset);
        let mut data = vec![0; read_len];
        self.fill_entries(*offset, &mut data)?;
        buf.write(&data);
        *offset += read_len;
        Ok(read_len)
    }

    fn write(&self, _buf: UserBuffer) -> SyscallRet {
        Err(SysErrNo::EBADF)
    }

    fn fstat(&self) -> Kstat {
        let size = self.size().unwrap_or(0);
        Kstat {
            st_mode: StMode::FREG.bits() | 0o444,
            st_nlink: 1,
            st_size: size as isize,
            st_blksize: PAGE_SIZE as i32,
            ..Kstat::default()
        }
    }

    fn path(&self) -> Cow<'_, str> {
        Cow::Borrowed(&self.path)
    }

    fn lseek(&self, offset: isize, whence: usize) -> SyscallRet {
        let mut current = self.offset.lock();
        let base = match whence {
            SEEK_SET => 0isize,
            SEEK_CUR => *current as isize,
            SEEK_END => self.size()? as isize,
            _ => return Err(SysErrNo::EINVAL),
        };
        let next = base.checked_add(offset).ok_or(SysErrNo::EINVAL)?;
        if next < 0 {
            return Err(SysErrNo::EINVAL);
        }
        *current = next as usize;
        Ok(*current)
    }

    fn poll(&self, events: PollEvents) -> PollEvents {
        if events.contains(PollEvents::IN) {
            PollEvents::IN
        } else {
            PollEvents::empty()
        }
    }
}
