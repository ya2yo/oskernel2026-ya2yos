//! memory related syscall

use alloc::format;
use log::{debug, warn};

use super::super::{MmapFlags, MmapProt};
use crate::{
    arch::memory_layout::{MAX_MMAP_SIZE, PAGE_SIZE},
    fs::File,
    mm::{
        copy_to_user, if_bad_address, insert_bad_address, remove_bad_address, shm_attach,
        shm_create, shm_drop, shm_find, MapArea, MapAreaType, MapPermission, MremapFlags, ShmFlags,
        VirtAddr, VirtPageNum,
    },
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
    let process = task.process.inner_lock();
    let memory_set = process.get_locked_memory_set_write();
    let len = page_round_up(len);
    // Reject unreasonably large single mmap requests.
    // Glibc on our char-device stdin probes available memory by
    // mmap'ing exponentially-growing anonymous regions.  Without a
    // per-request limit a 134 MiB mmap succeeds (virtual address
    // space is cheap) but touching its pages triggers page faults
    // that exhaust physical CMA frames before the caller can munmap.
    // CMA has ~78 MiB free after kernel/initproc allocations.
    // A single request larger than 64 MiB risks touching all its
    // pages and exhausting physical frames (as glibc probing does).
    if len > MAX_MMAP_SIZE / 4 {
        return Err(SysErrNo::ENOMEM);
    }
    if fd == usize::MAX {
        if !flags.contains(MmapFlags::MAP_ANONYMOUS) {
            return Err(SysErrNo::EBADF);
        }
        let rv = memory_set.mmap(addr, len, map_perm, flags, None, usize::MAX);
        if rv == 0 {
            if flags.contains(MmapFlags::MAP_FIXED_NOREPLACE) {
                return Err(SysErrNo::EEXIST);
            }
            return Err(SysErrNo::ENOMEM);
        }
        return Ok(rv);
    }
    if flags.contains(MmapFlags::MAP_ANONYMOUS) {
        //映射1字节没有任何权限的地址
        let rv = memory_set.mmap(0, 1, MapPermission::empty(), flags, None, usize::MAX);
        if rv == 0 {
            return Err(SysErrNo::ENOMEM);
        }
        insert_bad_address(rv);
        log::info!("bad address is 0x{:x}", rv);
        return Ok(rv);
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
    let task = current_task().unwrap();
    let process = task.process.inner_lock();
    let memory_set = process.get_locked_memory_set_write();
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
    let process = task.process.inner_lock();
    let memory_set = process.get_locked_memory_set_write();
    // 检查 old_addr + old_size 是否溢出
    let old_end = match old_addr.checked_add(old_size) {
        Some(v) => v,
        None => return Err(SysErrNo::EINVAL),
    };
    let old_area = match memory_set.get_mut().find_area_by_range(
        VirtAddr::from(old_addr).floor(),
        VirtAddr::from(old_end.saturating_sub(1)).ceil(),
    ) {
        Some(area) => area,
        None => return Err(SysErrNo::EFAULT),
    };
    if old_area.area_type != MapAreaType::Mmap {
        debug!("old_area.area_type != MapAreaType::Mmap");
        return Err(SysErrNo::EINVAL);
    }
    let old_flag = old_area.mmap_flags;
    let old_file = &old_area.mmap_file;
    let old_perm = old_area.map_perm;

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
    let map_perm: MapPermission = MmapProt::from_bits_truncate(prot).into();

    // debug!(
    //     "[sys_mprotect] addr is {:x}, len is {:#x}, map_perm is {:?}",
    //     addr, len, map_perm
    // );

    let task = current_task().unwrap();
    let process = task.process.inner_lock();
    let memory_set = process.get_locked_memory_set_write();
    let start_vpn = VirtAddr::from(addr).floor();
    let end_vpn = VirtAddr::from(end_addr).ceil();
    //修改各段的mappermission
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

/// 参考 https://man7.org/linux/man-pages/man2/shmget.2.html
pub fn sys_shmget(key: i32, size: usize, shmflag: i32) -> SyscallRet {
    const IPC_PRIVATE: i32 = 0;
    // 忽略权限位
    let flags = ShmFlags::from_bits(shmflag & !0x1ff).unwrap();
    match key {
        IPC_PRIVATE => Ok(shm_create(size)),
        key if key > 0 => {
            if shm_find(key as usize) {
                if flags.contains(ShmFlags::IPC_CREAT | ShmFlags::IPC_EXCL) {
                    Err(SysErrNo::EEXIST)
                } else {
                    Ok(key as usize)
                }
            } else {
                if flags.contains(ShmFlags::IPC_CREAT) {
                    Ok(shm_create(size))
                } else {
                    Err(SysErrNo::ENOENT)
                }
            }
        }
        _ => Err(SysErrNo::ENOENT),
    }
}

/// 参考 https://man7.org/linux/man-pages/man2/shmat.2.html
pub fn sys_shmat(shmid: i32, shmaddr: usize, shmflag: i32) -> SyscallRet {
    let mut permission = MapPermission::U | MapPermission::R;
    if shmflag == 0 {
        permission |= MapPermission::W | MapPermission::X
    } else {
        let shmflg = ShmFlags::from_bits(shmflag).unwrap();
        if shmflg.contains(ShmFlags::SHM_EXEC) {
            permission |= MapPermission::X;
        }
        if !shmflg.contains(ShmFlags::SHM_RDONLY) {
            permission |= MapPermission::W;
        }
    }

    match shmid {
        key if key < 0 => Err(SysErrNo::EINVAL),
        _ => shm_attach(shmid as usize, shmaddr, permission),
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
    let proc = task.process.inner_lock();
    let memory_set = proc.get_locked_memory_set_read();

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

/// 参考 https://man7.org/linux/man-pages/man2/shmctl.2.html
pub fn sys_shmctl(shmid: i32, cmd: i32, _buf: usize) -> SyscallRet {
    const IPC_RMID: i32 = 0;
    match cmd {
        IPC_RMID => {
            shm_drop(shmid as usize);
            Ok(0)
        }
        _ => {
            warn!("[sys_shmctl] unsupport cmd");
            Err(SysErrNo::ENOSYS)
        }
    }
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
