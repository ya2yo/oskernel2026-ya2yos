use crate::{
    arch::{memory_layout::PAGE_SIZE, time::get_ticks},
    mm::{MapPermission, MemorySet, UserBuffer},
};

// 该文件创建形如/proc/xxx的文件
use super::*;
use alloc::{format, string::String, vec::Vec};

fn format_map_perm(perm: MapPermission) -> String {
    let mut s = String::with_capacity(4);
    s.push(if perm.contains(MapPermission::R) {
        'r'
    } else {
        '-'
    });
    s.push(if perm.contains(MapPermission::W) {
        'w'
    } else {
        '-'
    });
    s.push(if perm.contains(MapPermission::X) {
        'x'
    } else {
        '-'
    });
    s.push('p');
    s
}

fn write_kernel_file(file: &dyn File, data: &mut String) -> Result<usize, SysErrNo> {
    let mut vec = Vec::new();
    unsafe {
        let bytes = data.as_bytes_mut();
        vec.push(core::slice::from_raw_parts_mut(
            bytes.as_mut_ptr(),
            bytes.len(),
        ));
    }
    file.write(UserBuffer::new(vec))
}

fn format_status(pid: usize, ppid: usize, comm: &str, memory_set: &MemorySet) -> String {
    let vm_size = memory_set.virtual_size_kb();
    let vm_rss = memory_set.resident_size_kb();
    format!(
        "VmSwap:\t       0 kB\n\
VmHWM:\t{:8} kB\n\
VmRSS:\t{:8} kB\n\
Name:\t{}\n\
State:\tS (sleeping)\n\
Tgid:\t{}\n\
Pid:\t{}\n\
PPid:\t{}\n\
VmPeak:\t{:8} kB\n\
VmSize:\t{:8} kB\n\
VmHWM:\t{:8} kB\n\
VmRSS:\t{:8} kB\n",
        vm_rss, vm_rss, comm, pid, pid, ppid, vm_size, vm_size, vm_rss, vm_rss
    )
}

fn format_stat(pid: usize, ppid: usize, state: char, comm: &str, memory_set: &MemorySet) -> String {
    let vsize = memory_set.virtual_size_kb() * 1024;
    let rss_pages = memory_set.resident_size_kb() * 1024 / PAGE_SIZE;
    format!(
        "{} ({}) {} {} 0 0 0 0 0 0 0 0 0 0 0 0 0 0 1 0 0 {} {} {} 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0\n",
        pid,
        comm,
        state,
        ppid,
        get_ticks(),
        vsize,
        rss_pages
    )
}

pub fn create_proc_dir_and_file(
    pid: usize,
    ppid: usize,
    comm: &str,
    memory_set: &MemorySet,
) -> Result<(), SysErrNo> {
    let procdir = open(
        format!("/proc/{}", pid).as_str(),
        OpenFlags::O_DIRECTORY | OpenFlags::O_CREATE | OpenFlags::O_RDWR,
        DEFAULT_DIR_MODE,
    )?
    .file()?;
    // 强制刷盘确保目录创建持久化，否则后续在目录内创建的文件可能不可见
    procdir.inode.sync();

    //创建进程状态文件/proc/<pid>/stat
    let statfile = open(
        format!("/proc/{}/stat", pid).as_str(),
        OpenFlags::O_CREATE | OpenFlags::O_RDWR | OpenFlags::O_TRUNC,
        DEFAULT_FILE_MODE,
    )
    .unwrap()
    .file()?;
    let mut statinfo = format_stat(pid, ppid, 'S', comm, memory_set);
    write_kernel_file(statfile.as_ref(), &mut statinfo)?;
    statfile.inode.sync();

    //创建进程状态文件/proc/<pid>/status
    let statusfile = open(
        format!("/proc/{}/status", pid).as_str(),
        OpenFlags::O_CREATE | OpenFlags::O_RDWR | OpenFlags::O_TRUNC,
        DEFAULT_FILE_MODE,
    )?
    .file()?;
    let mut statusinfo = format_status(pid, ppid, comm, memory_set);
    write_kernel_file(statusfile.as_ref(), &mut statusinfo)?;
    statusfile.inode.sync();

    //创建进程内存映射文件/proc/<pid>/maps
    let mapsfile = open(
        format!("/proc/{}/maps", pid).as_str(),
        OpenFlags::O_CREATE | OpenFlags::O_RDWR,
        DEFAULT_FILE_MODE,
    )?
    .file()?;
    let mut mapsinfo = String::new();
    for area in &memory_set.get_ref().areas {
        let start = area.vpn_range.start().0 * PAGE_SIZE;
        let end = area.vpn_range.end().0 * PAGE_SIZE;
        let perm = format_map_perm(area.map_perm);
        let offset = area.mmap_file.offset;
        let pathname = if let Some(ref file) = area.mmap_file.file {
            file.inode.path()
        } else {
            String::new()
        };
        mapsinfo.push_str(&format!(
            "{:016x}-{:016x} {} {:08x} 00:00 0 {}\n",
            start, end, perm, offset, pathname
        ));
    }
    write_kernel_file(mapsfile.as_ref(), &mut mapsinfo)?;
    mapsfile.inode.sync();

    Ok(())
}

pub fn refresh_proc_stat(
    pid: usize,
    ppid: usize,
    state: char,
    comm: &str,
    memory_set: &MemorySet,
) -> Result<(), SysErrNo> {
    let statfile = open(
        format!("/proc/{}/stat", pid).as_str(),
        OpenFlags::O_CREATE | OpenFlags::O_RDWR | OpenFlags::O_TRUNC,
        DEFAULT_FILE_MODE,
    )?
    .file()?;
    let mut statinfo = format_stat(pid, ppid, state, comm, memory_set);
    write_kernel_file(statfile.as_ref(), &mut statinfo)?;
    statfile.inode.sync();
    Ok(())
}

pub fn refresh_proc_status(
    pid: usize,
    ppid: usize,
    comm: &str,
    memory_set: &MemorySet,
) -> Result<(), SysErrNo> {
    let statusfile = open(
        format!("/proc/{}/status", pid).as_str(),
        OpenFlags::O_CREATE | OpenFlags::O_RDWR | OpenFlags::O_TRUNC,
        DEFAULT_FILE_MODE,
    )?
    .file()?;
    let mut statusinfo = format_status(pid, ppid, comm, memory_set);
    write_kernel_file(statusfile.as_ref(), &mut statusinfo)?;
    statusfile.inode.sync();
    Ok(())
}

pub fn remove_proc_dir_and_file(pid: usize) {
    superblock_root_inode().unlink(format!("/proc/{}/maps", pid).as_str());
    FsIndex::remove_inode_idx(format!("/proc/{}/maps", pid).as_str());
    superblock_root_inode().unlink(format!("/proc/{}/status", pid).as_str());
    FsIndex::remove_inode_idx(format!("/proc/{}/status", pid).as_str());
    superblock_root_inode().unlink(format!("/proc/{}/stat", pid).as_str());
    FsIndex::remove_inode_idx(format!("/proc/{}/stat", pid).as_str());
    superblock_root_inode().unlink(format!("/proc/{}", pid).as_str());
    FsIndex::remove_inode_idx(format!("/proc/{}", pid).as_str());
}
