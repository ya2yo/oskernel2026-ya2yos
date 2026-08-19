//! `/proc/<pid>/pagemap` 的动态文件实现。
//!
//! 该文件把进程地址空间中的每个虚拟页编码为一个 64 位条目：低 55 位保存
//! 物理页帧号（PFN），最高位表示该虚拟页当前是否存在于页表中。与普通文件
//! 不同，pagemap 不在磁盘上保存内容，而是在每次读取时根据关联的
//! [`MemorySet`] 即时生成结果。
//!
//! 每个打开对象拥有独立的字节偏移，允许非 8 字节对齐的分段读取；未映射页
//! 返回零条目，已映射页设置 present 位并编码 PFN。该视图只读、始终报告
//! `POLLIN` 就绪，逻辑大小随最高 VMA 端点动态计算。

use crate::{
    arch::memory_layout::PAGE_SIZE,
    fs::{File, Kstat, StMode, SEEK_CUR, SEEK_END, SEEK_SET},
    mm::{MemorySet, UserBuffer, VirtPageNum},
    syscall::PollEvents,
    utils::{SysErrNo, SyscallRet},
};
use alloc::{borrow::Cow, string::String, sync::Arc, vec};
use spin::Mutex;

/// 每个虚拟页在 pagemap 中占用的字节数。
///
/// 条目按原生字节序编码，以便直接兼容当前架构上的用户态读取方式。
const PAGEMAP_ENTRY_SIZE: usize = core::mem::size_of::<u64>();
/// pagemap 条目中用于保存 PFN 的低 55 位掩码。
const PAGEMAP_PFN_MASK: u64 = (1u64 << 55) - 1;
/// pagemap 条目中表示虚拟页已映射到物理页的最高位。
const PAGEMAP_PRESENT: u64 = 1u64 << 63;

/// 只读、按需生成的 `/proc/<pid>/pagemap` 文件视图。
///
/// 每个文件描述符拥有独立的读取偏移，但所有读取都观察同一个进程的
/// [`MemorySet`]。对于虚拟页 `VPN`，文件偏移 `VPN * 8` 对应一个原生字节序的
/// 64 位条目；如果页表中存在该页，条目设置 [`PAGEMAP_PRESENT`] 并写入物理页
/// 帧号，否则条目为零。
///
/// ext4 后端并不能高效地实现稀疏 `truncate()`：如果把 pagemap 扩展到进程
/// 最高的用户 VMA，每次 fork 都可能为其分配数百 MiB。因此 ext4 中只保留
/// proc 目录项，具体条目在描述符读取时即时合成。
pub struct PagemapFile {
    /// 文件关联的进程地址空间和页表。
    memory_set: Arc<MemorySet>,
    /// 该动态文件对外呈现的 proc 路径。
    path: String,
    /// 当前描述符的文件偏移，以字节为单位。
    offset: Mutex<usize>,
}

impl PagemapFile {
    /// 为指定地址空间创建一个新的 pagemap 文件描述符对象。
    ///
    /// 新对象的读取偏移从零开始；`memory_set` 以 [`Arc`] 持有，使得文件描述符
    /// 存活期间地址空间不会被释放。
    pub fn open(memory_set: Arc<MemorySet>, path: String) -> Arc<Self> {
        Arc::new(Self {
            memory_set,
            path,
            offset: Mutex::new(0),
        })
    }

    /// 计算动态文件的逻辑大小。
    ///
    /// 大小由地址空间中结束地址最高的 VMA 决定，即最高 VPN（不包含）乘以
    /// 单个条目的大小。未映射的 VPN 也属于这个逻辑范围，读取时返回零条目。
    /// 算术溢出无法表示为合法文件大小，因此转换为 `EFBIG`。
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

    /// 从指定文件偏移开始生成 pagemap 条目，并填充到 `data`。
    ///
    /// 一次读取可能从一个 8 字节条目的中间开始，也可能在条目中间结束，
    /// 因此这里只复制每个受影响条目的对应字节。对于没有 VMA 或没有页表
    /// 映射的虚拟页，使用全零条目；对于已映射页，通过页表查询物理页号。
    /// `data` 的内容只包含当前读取范围，不会扩大到完整条目边界。
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
    /// pagemap 仅支持读取动态生成的条目。
    fn readable(&self) -> bool {
        true
    }

    /// pagemap 不接受写入，写操作由 [`Self::write`] 返回 `EBADF`。
    fn writable(&self) -> bool {
        false
    }

    /// 按当前文件偏移读取并生成 pagemap 原始条目。
    ///
    /// 读取不会超过动态文件的逻辑大小；到达文件末尾后返回零。用户缓冲区
    /// 只接收本次请求覆盖的字节，成功读取后文件偏移向前推进相同长度。
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

    /// 拒绝写入；pagemap 是只读的内核视图。
    fn write(&self, _buf: UserBuffer) -> SyscallRet {
        Err(SysErrNo::EBADF)
    }

    /// 返回 pagemap 的逻辑属性和动态大小。
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

    /// 返回该动态文件对外暴露的 proc 路径。
    fn path(&self) -> Cow<'_, str> {
        Cow::Borrowed(&self.path)
    }

    /// 按 `SEEK_SET`、`SEEK_CUR` 或 `SEEK_END` 调整当前读取偏移。
    ///
    /// 偏移可以位于条目内部，以支持与普通文件一致的字节粒度读取；负数
    /// 结果和整数溢出都会返回 `EINVAL`。
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

    /// 对 `POLLIN`/可读事件报告就绪；pagemap 始终可以被读取。
    fn poll(&self, events: PollEvents) -> PollEvents {
        if events.contains(PollEvents::IN) {
            PollEvents::IN
        } else {
            PollEvents::empty()
        }
    }
}
