//! memory related syscall

use alloc::format;
use linux_raw_sys::general::{
    MADV_DONTNEED, MADV_NORMAL, MADV_RANDOM, MADV_SEQUENTIAL, MADV_WILLNEED, MAP_SHARED_VALIDATE,
    MAP_TYPE,
};
use log::{debug, warn};

use crate::{
    arch::memory_layout::{MAX_MMAP_SIZE, PAGE_SIZE, USER_SPACE_SIZE},
    fs::{get_devno, File},
    mm::{
        copy_to_user, if_bad_address, remove_bad_address, MapArea, MapAreaType, MapPermission,
        MremapFlags, VirtAddr, VirtPageNum,
    },
    signal::{send_signal_to_thread, SigSet},
    syscall::options::{MmapFlags, MmapProt},
    task::{self, current_task},
    utils::{page_round_up, SysErrNo, SyscallRet},
};

/// 参考 https://man7.org/linux/man-pages/man2/mmap.2.html
pub fn sys_mmap(
    addr: usize,
    len: usize,
    prot: u32,
    flags: u32,
    fd: usize,
    off: usize,
) -> SyscallRet {
    let raw_flags = flags;
    let flags = MmapFlags::from_bits_truncate(raw_flags);
    debug!(
        "[sysmap] addr={:#x},len={:#x},prot={:#x},flags={:?},fd={},off={:#x}",
        addr, len, prot, flags, fd, off
    );
    // Linux ignores unknown mmap bits for MAP_SHARED/MAP_PRIVATE, but
    // MAP_SHARED_VALIDATE turns them into a strict capability check.
    if raw_flags & MAP_TYPE == MAP_SHARED_VALIDATE && raw_flags & !MmapFlags::all().bits() != 0 {
        return Err(SysErrNo::EOPNOTSUPP);
    }
    if flags
        .intersection(
            MmapFlags::MAP_PRIVATE | MmapFlags::MAP_SHARED | MmapFlags::MAP_SHARED_VALIDATE,
        )
        .is_empty()
    {
        return Err(SysErrNo::EINVAL);
    }
    // mmap08 passes a closed fd together with a zero length and expects the
    // file-descriptor error to take precedence for a valid file-backed mapping.
    if !flags.contains(MmapFlags::MAP_ANONYMOUS) && fd == usize::MAX {
        return Err(SysErrNo::EBADF);
    }
    if len <= 0 {
        // must be greater than 0
        return Err(SysErrNo::EINVAL);
    }
    let map_perm: MapPermission = MmapProt::from_bits_truncate(prot).into();
    // 使用 from_bits_truncate 忽略未知标志位，与 Linux 内核行为一致
    // flags=0x4022导致问题
    // 不对啊，1<<14这一位没用啊？

    let task = current_task().unwrap();
    let process = &task.process;
    let memory_set = process.memory_set_arc();

    let len = page_round_up(len);
    // Reject requests beyond the configured per-process mmap budget.
    if len > MAX_MMAP_SIZE {
        return Err(SysErrNo::ENOMEM);
    }
    if flags.contains(MmapFlags::MAP_ANONYMOUS) {
        let rv = memory_set.mmap(addr, len, map_perm, flags, None, usize::MAX);
        if rv == 0 {
            if flags.contains(MmapFlags::MAP_FIXED_NOREPLACE) {
                return Err(SysErrNo::EEXIST);
            }
            return Err(SysErrNo::ENOMEM);
        }
        return Ok(rv);
    }

    if fd == usize::MAX {
        return Err(SysErrNo::EBADF);
    }
    let descriptor = process.fd_table.get(fd)?;
    let file = match descriptor.file() {
        Ok(file) => {
            // 访问权限检查
            if !file.readable() {
                return Err(SysErrNo::EACCES);
            }
            if flags.contains(MmapFlags::MAP_SHARED)
                && map_perm.contains(MapPermission::W)
                && !file.writable()
            {
                return Err(SysErrNo::EACCES);
            }
            Some(file)
        }
        Err(SysErrNo::EINVAL) => {
            let device = descriptor.abs()?;
            if device.fstat().st_rdev != get_devno("/dev/zero") {
                return Err(SysErrNo::EINVAL);
            }
            if map_perm.contains(MapPermission::R) && !device.readable()
                || flags.contains(MmapFlags::MAP_SHARED)
                    && map_perm.contains(MapPermission::W)
                    && !device.writable()
            {
                return Err(SysErrNo::EACCES);
            }
            None
        }
        Err(err) => return Err(err),
    };
    let rv = memory_set.mmap(addr, len, map_perm, flags, file, off);
    debug!("[sys_mmap] alloc addr={:#x}", rv);
    if rv == 0 {
        if flags.contains(MmapFlags::MAP_FIXED_NOREPLACE) {
            return Err(SysErrNo::EEXIST);
        }
        return Err(SysErrNo::ENOMEM);
    }
    Ok(rv)
}

/// 参考 https://man7.org/linux/man-pages/man2/munmap.2.html
pub fn sys_munmap(addr: usize, len: usize) -> SyscallRet {
    debug!("[sys_munmap] addr={:#x}, len={:#x}", addr, len);
    // addr 必须页对齐
    if addr % PAGE_SIZE != 0 {
        return Err(SysErrNo::EINVAL);
    }
    #[cfg(target_arch = "loongarch64")]
    {
        let len = page_round_up(len);
        let end = addr.checked_add(len).ok_or(SysErrNo::EINVAL)?;
        if VirtAddr::try_from(addr).is_none() || (len != 0 && VirtAddr::try_from(end - 1).is_none())
        {
            return Err(SysErrNo::EINVAL);
        }
    }
    let task = current_task().unwrap();
    let process = &task.process;
    let memory_set = process.memory_set_arc();
    let len = page_round_up(len);
    if if_bad_address(addr) {
        remove_bad_address(addr);
    }
    match memory_set.munmap(addr, len) {
        Err(SysErrNo::ENOSPC) => {
            // A shared file mapping can discover exhausted backing blocks only
            // while dirty pages are written during munmap. Linux reports this
            // as SIGBUS rather than turning the unmap into an ordinary error.
            send_signal_to_thread(task.tid(), SigSet::SIGBUS);
            Ok(0)
        }
        result => result,
    }
}

/// https://www.man7.org/linux/man-pages/man2/mremap.2.html
pub fn sys_mremap(
    old_addr: usize,
    old_size: usize,
    new_size: usize,
    flags: i32,
    new_addr: usize,
) -> SyscallRet {
    let flags_bitmap = MremapFlags::from_bits(flags).ok_or(SysErrNo::EINVAL)?;
    debug!(
        "[sys_mremap] old_addr={:#x}, old_size={:#X}, new_size={:#x}, flags={:?}",
        old_addr, old_size, new_size, flags_bitmap
    );
    let may_move = flags_bitmap.contains(MremapFlags::MAYMOVE);
    let fixed = flags_bitmap.contains(MremapFlags::FIXED);
    let dont_unmap = flags_bitmap.contains(MremapFlags::DONTUNMAP);
    if dont_unmap {
        return Err(SysErrNo::ENOSYS);
    }
    if fixed && !may_move {
        return Err(SysErrNo::EINVAL);
    }
    if old_addr % PAGE_SIZE != 0 || old_size == 0 || new_size == 0 {
        return Err(SysErrNo::EINVAL);
    }
    if fixed && may_move && new_addr % PAGE_SIZE != 0 {
        return Err(SysErrNo::EINVAL);
    }
    let old_len = old_size
        .checked_add(PAGE_SIZE - 1)
        .map(|size| size / PAGE_SIZE * PAGE_SIZE)
        .ok_or(SysErrNo::EINVAL)?;
    let new_len = new_size
        .checked_add(PAGE_SIZE - 1)
        .map(|size| size / PAGE_SIZE * PAGE_SIZE)
        .ok_or(SysErrNo::EINVAL)?;
    let old_end = old_addr.checked_add(old_len).ok_or(SysErrNo::EINVAL)?;
    if old_end > USER_SPACE_SIZE
        || VirtAddr::try_from(old_addr).is_none()
        || VirtAddr::try_from(old_end - 1).is_none()
    {
        return Err(SysErrNo::EINVAL);
    }

    let task = current_task().unwrap();
    let memory_set = task.process.memory_set_arc();
    let result = memory_set.with_mut(|memory_set| {
        if may_move {
            memory_set.mremap_maymove(old_addr, old_len, new_len, new_addr, fixed)
        } else {
            memory_set.mremap_in_place(old_addr, old_len, new_len)
        }
    });
    if result.is_ok() && if_bad_address(old_addr) {
        remove_bad_address(old_addr);
    }
    result
}

/// 参考 https://man7.org/linux/man-pages/man2/mprotect.2.html
///
/// 修改调用进程虚拟地址空间 `[addr, addr+len)` 范围内的访问权限。
///
/// # 参数校验
/// - `addr` 和 `len` 必须按页对齐，否则返回 `EINVAL`。
/// - 检查 `addr + len` 是否溢出，溢出返回 `EINVAL`。
///
/// # 权限转换
/// 将用户传入的 `prot` 位掩码（来自 `MmapProt`）转换为内核内部的 `MapPermission`。
///
/// # 执行流程
/// 1. 根据 `addr`/`len` 计算虚拟页号范围 `[start_vpn, end_vpn)`。
/// 2. 调用 `MemorySet::mprotect` 修改逻辑段（MapArea）权限并更新硬件页表项。
/// 3. 跨 area 边界的情况由 `MemorySetInner::mprotect` 内部的拆分逻辑处理。
///
/// # 注意
/// - 目前为简化实现，不检查范围是否全部落在已映射区域内（Linux 对此返回 `ENOMEM`）。
/// - `prot` 中不被支持的位会被 `from_bits_truncate` 静默丢弃。
pub fn sys_mprotect(addr: usize, len: usize, prot: u32) -> SyscallRet {
    if (addr % PAGE_SIZE != 0) || (len % PAGE_SIZE != 0) {
        println!("sys_mprotect: not align");
        return Err(SysErrNo::EINVAL);
    }
    // 检查 addr + len 是否溢出
    let end_addr = match addr.checked_add(len) {
        Some(v) => v,
        None => return Err(SysErrNo::EINVAL),
    };
    // 将 POSIX prot 标志转换为内核内部的 MapPermission
    let map_perm: MapPermission = MmapProt::from_bits_truncate(prot).into();

    let task = current_task().unwrap();
    let process = &task.process;
    let memory_set = process.memory_set_arc();
    let start_vpn = VirtAddr::from(addr).floor();
    let end_vpn = VirtAddr::from(end_addr).ceil();
    // 修改各逻辑段的权限并更新页表
    memory_set.mprotect(start_vpn, end_vpn, map_perm);
    Ok(0)
}

/// 参考 https://man7.org/linux/man-pages/man2/madvise.2.html
pub fn sys_madvise(addr: usize, len: usize, advice: usize) -> SyscallRet {
    // Keep the non-destructive hints compatible with libc users. DONTNEED is
    // handled by MemorySet so it can drop the resident frames under its lock.
    match advice as u32 {
        MADV_NORMAL | MADV_RANDOM | MADV_SEQUENTIAL | MADV_WILLNEED | MADV_DONTNEED => {}
        _ => return Err(SysErrNo::EINVAL),
    }
    if addr % PAGE_SIZE != 0 {
        return Err(SysErrNo::EINVAL);
    }
    if len == 0 {
        return Ok(0);
    }
    let end = addr.checked_add(len).ok_or(SysErrNo::EINVAL)?;
    if VirtAddr::try_from(addr).is_none() || VirtAddr::try_from(end - 1).is_none() {
        return Err(SysErrNo::EINVAL);
    }

    let task = current_task().unwrap();
    let memory_set = task.process.memory_set_arc();
    match advice as u32 {
        MADV_DONTNEED => memory_set.discard_madvise_pages(addr, len),
        // These are ordering/readahead hints. This kernel has no swap or
        // readahead policy, so accepting them has no resident-page effect.
        MADV_NORMAL | MADV_RANDOM | MADV_SEQUENTIAL | MADV_WILLNEED => {
            memory_set.validate_madvise_range(addr, len)
        }
        _ => unreachable!("madvise advice was validated above"),
    }
}

/// 参考 https://man7.org/linux/man-pages/man2/mincore.2.html
///
/// 查询地址范围内各页是否驻留在物理内存中。
/// vec[i] 的最低位为 1 表示该页在 RAM 中（已分配物理帧）。
/// 本内核无 swap，因此已分配帧的页面始终返回 1，惰性分配的页面返回 0。
pub fn sys_mincore(addr: usize, length: usize, vec: *mut u8) -> SyscallRet {
    // EFAULT: vec 必须为有效用户态可写地址
    if vec.is_null() || (vec as isize) <= 0 || if_bad_address(vec as usize) {
        return Err(SysErrNo::EFAULT);
    }

    // EINVAL: addr 必须页对齐
    if addr % PAGE_SIZE != 0 {
        return Err(SysErrNo::EINVAL);
    }

    // 长度为 0 直接成功
    if length == 0 {
        return Ok(0);
    }

    // EINVAL: 溢出检查，addr + length 不能溢出
    if addr.checked_add(length).is_none() {
        return Err(SysErrNo::EINVAL);
    }

    let task = current_task().unwrap();
    let proc = &task.process;
    let memory_set = proc.memory_set_arc();

    // ENOMEM: 地址范围必须全部在已有映射内
    if !memory_set.check_user_range(addr, length, MapPermission::R) {
        return Err(SysErrNo::ENOMEM);
    }

    // 计算需要的 vec 大小
    let num_pages = (length + PAGE_SIZE - 1) / PAGE_SIZE;

    let mut vec_data = alloc::vec![0u8; num_pages];

    for i in 0..num_pages {
        let page_addr = addr + i * PAGE_SIZE;
        let vpn = VirtAddr::from(page_addr).floor();
        // translate 返回 Some 表示页表中有映射（物理帧已分配）
        if memory_set.translate(vpn).is_some() {
            vec_data[i] = 1;
        }
    }

    // 写回用户态 vec
    copy_to_user(&memory_set, vec as usize, &vec_data)?;

    Ok(0)
}

// System V 消息队列 (msgget/msgsnd/msgrcv/msgctl) — 桩实现
/// https://man7.org/linux/man-pages/man2/msgget.2.html
/// 获取 System V 消息队列标识符（通过 key 创建或打开）
pub fn sys_msgget(_key: i32, _msgflg: i32) -> SyscallRet {
    warn!("[sys_msgget] not implement!");
    Err(SysErrNo::ENOSYS)
}

/// https://man7.org/linux/man-pages/man2/msgsnd.2.html
/// 向消息队列发送消息
pub fn sys_msgsnd(_msqid: i32, _msgp: *const u8, _msgsz: usize, _msgflg: i32) -> SyscallRet {
    warn!("[sys_msgsnd] not implement!");
    Err(SysErrNo::ENOSYS)
}

/// https://man7.org/linux/man-pages/man2/msgrcv.2.html
/// 从消息队列接收消息
pub fn sys_msgrcv(
    _msqid: i32,
    _msgp: *mut u8,
    _msgsz: usize,
    _msgtyp: i64,
    _msgflg: i32,
) -> SyscallRet {
    warn!("[sys_msgrcv] not implement!");
    Err(SysErrNo::ENOSYS)
}

/// https://man7.org/linux/man-pages/man2/msgctl.2.html
/// 消息队列控制操作
pub fn sys_msgctl(_msqid: i32, _cmd: i32, _buf: *mut u8) -> SyscallRet {
    warn!("[sys_msgctl] not implement!");
    Err(SysErrNo::ENOSYS)
}
