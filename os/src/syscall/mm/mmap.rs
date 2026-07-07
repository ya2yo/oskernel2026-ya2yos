//! memory related syscall

use alloc::format;
use log::{debug, warn};

use crate::{
    arch::memory_layout::{MAX_MMAP_SIZE, PAGE_SIZE},
    fs::File,
    mm::{
        copy_to_user, if_bad_address, remove_bad_address, MapArea, MapAreaType, MapPermission,
        MremapFlags, VirtAddr, VirtPageNum,
    },
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
    debug!(
        "[sysmap] addr={:#x},len={},prot={:#x},flags={:#x},fd={},off={}",
        addr, len, prot, flags, fd, off
    );
    let map_perm: MapPermission = MmapProt::from_bits_truncate(prot).into();
    // 使用 from_bits_truncate 忽略未知标志位，与 Linux 内核行为一致
    let flags = MmapFlags::from_bits_truncate(flags);
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
    // check fd and map_permission
    let file = process.fd_table.get(fd)?.file()?;
    // 读写权限
    if map_perm.contains(MapPermission::R) && !file.readable()
        || flags.contains(MmapFlags::MAP_SHARED)
            && map_perm.contains(MapPermission::W)
            && !file.writable()
    {
        return Err(SysErrNo::EACCES);
    }
    let rv = memory_set.mmap(addr, len, map_perm, flags, Some(file), off);
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
    memory_set.munmap(addr, len)
}

/// Add by HXC
pub fn sys_mremap(
    old_addr: usize,
    old_size: usize,
    new_size: usize,
    flags: i32,
    _new_addr: usize,
) -> SyscallRet {
    let flags_bitmap = MremapFlags::from_bits(flags).expect("Invalid flags on mremap!");
    // debug!(
    //     "sys_mremap: old_addr={:#x}, old_size={}, new_size={}, new_addr={:#x}",
    //     old_addr, old_size, new_size, new_addr
    // );
    // debug!("flags={:?}", flags_bitmap);
    // 允许内核在原地址空间不足时，将内存区域移动到新的虚拟地址。此时返回值应为移动后的地址。此时new_addr参数应该被忽视
    // 如果为false，则表示必须原地扩展/收缩
    let may_move = flags_bitmap.contains(MremapFlags::MAYMOVE);

    // 强制将内存重新映射到指定的新地址new_addr​（需配合MREMAP_MAYMOVE使用）
    // 如果为true，则may_move必须为true
    // 如果为false，则new_addr则是建议地址，不强制
    let fixed = flags_bitmap.contains(MremapFlags::FIXED);

    // 如果存在，则并不调整原来的map，而是创建一个新的[new_addr, new_addr+new_size]虚拟地址空间，映射到原来的物理地址空间
    let dont_unmap = flags_bitmap.contains(MremapFlags::DONTUNMAP);
    if dont_unmap {
        warn!("sys_munmap for DONTUNMAP unimplemented!");
        return Err(SysErrNo::ENOSYS);
    }
    if fixed && !may_move {
        return Err(SysErrNo::EINVAL);
    }
    let task = current_task().unwrap();
    let process = &task.process;
    let memory_set = process.memory_set_arc();
    // 检查 old_addr + old_size 是否溢出
    let old_end = match old_addr.checked_add(old_size) {
        Some(v) => v,
        None => return Err(SysErrNo::EINVAL),
    };
    let old_range = (
        VirtAddr::from(old_addr).floor(),
        VirtAddr::from(old_end.saturating_sub(1)).ceil(),
    );
    let (old_flag, old_file, old_perm) = memory_set.with_ref(|ms| {
        let old_area = ms
            .areas
            .iter()
            .find(|area| area.vpn_range.range() == old_range)
            .ok_or(SysErrNo::EFAULT)?;
        if old_area.area_type != MapAreaType::Mmap {
            debug!("old_area.area_type != MapAreaType::Mmap");
            return Err(SysErrNo::EINVAL);
        }
        Ok((
            old_area.mmap_flags,
            old_area.mmap_file.clone(),
            old_area.map_perm,
        ))
    })?;

    if fixed {
        warn!("fixed not implement");
        return Err(SysErrNo::ENOSYS);
    } else if may_move {
        // sys_munmap(old_addr, old_size);
        // debug!("[sys_munmap] addr={:#x}, len={:#x}", addr, len);
        if if_bad_address(old_addr) {
            remove_bad_address(old_addr);
        }
        memory_set.munmap(old_addr, page_round_up(old_size));
        // sys_mmap(old_addr, new_len, prot, flags, fd, off)
        let file = &old_file.file;

        if let Some(inode) = file {
            // 读写权限
            if old_perm.contains(MapPermission::R) && !inode.readable()
                || old_flag.contains(MmapFlags::MAP_SHARED)
                    && old_perm.contains(MapPermission::W)
                    && !inode.writable()
            {
                return Err(SysErrNo::EPERM);
            }
        }

        let rv = memory_set.mmap(
            old_addr,
            new_size,
            old_perm,
            old_flag,
            file.clone(),
            old_file.offset,
        );

        return Ok(rv);
    } else {
        // fixed == may_move == 0
        return Err(SysErrNo::ENOSYS);
    }
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
pub fn sys_madvise(_addr: usize, _len: usize, _advice: usize) -> SyscallRet {
    //伪实现，该系统调用用于给内存提建议
    // debug!(
    //     "[sys_madvise] addr is {}, len is {}, advice is {}",
    //     addr, len, advice
    // );
    Ok(0)
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
