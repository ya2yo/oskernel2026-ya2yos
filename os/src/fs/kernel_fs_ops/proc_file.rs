use crate::{
    arch::{memory_layout::PAGE_SIZE, time::get_ticks},
    fs::DENTRY_CACHE,
    mm::{MapPermission, MemorySet, UserBuffer},
    syscall::MmapFlags,
    utils::SysErrNo,
};

// 该文件创建形如/proc/xxx的文件
use super::*;
use alloc::{collections::BTreeSet, format, string::String, vec::Vec};
use spin::{Lazy, Mutex};

/// PIDs with a live process entry under `/proc`.
///
/// The directory is registered at clone time but materialized in EXT4 only
/// when a caller actually opens the task path or enumerates `/proc`.
static PROC_TASKS: Lazy<Mutex<BTreeSet<usize>>> = Lazy::new(|| Mutex::new(BTreeSet::new()));

fn register_proc_task(pid: usize) {
    PROC_TASKS.lock().insert(pid);
}

fn unregister_proc_task(pid: usize) {
    PROC_TASKS.lock().remove(&pid);
}

/// Ensure that a registered process has a physical `/proc/<pid>` directory.
pub fn ensure_proc_dir(pid: usize) -> Result<(), SysErrNo> {
    if !PROC_TASKS.lock().contains(&pid) {
        return Err(SysErrNo::ENOENT);
    }

    let path = format!("/proc/{}", pid);
    if FsIndex::find_inode_idx(&path).is_some() {
        return Ok(());
    }

    let parent = match FsIndex::find_inode_idx("/proc") {
        Some(parent) => parent,
        None => {
            let parent = superblock_root_inode().find("/proc", OpenFlags::O_DIRECTORY, 0)?;
            FsIndex::insert_inode_idx("/proc", parent)
        }
    };
    let inode = parent.create_dir_fast(&path)?;
    let inode = FsIndex::insert_inode_idx(&path, inode);
    let child_name = format!("{}", pid);
    DENTRY_CACHE.insert_positive(&parent, &child_name, inode);
    Ok(())
}

/// Materialize all live task directories before `/proc` is enumerated.
pub fn materialize_proc_dirs() -> Result<(), SysErrNo> {
    let pids = PROC_TASKS.lock().iter().copied().collect::<Vec<_>>();
    for pid in pids {
        ensure_proc_dir(pid)?;
    }
    Ok(())
}

/// Materialize the task directory encoded by a proc path, if present.
pub fn ensure_proc_path(path: &str) -> Result<(), SysErrNo> {
    let Some(rest) = path.strip_prefix("/proc/") else {
        return Ok(());
    };
    let component = rest.split('/').next().unwrap_or("");
    if component.is_empty() || !component.as_bytes().iter().all(|byte| byte.is_ascii_digit()) {
        return Ok(());
    }
    let pid = component.parse::<usize>().map_err(|_| SysErrNo::ENOENT)?;
    ensure_proc_dir(pid)
}

fn format_map_perm(perm: MapPermission, flags: MmapFlags) -> String {
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
    s.push(if flags.contains(MmapFlags::MAP_SHARED) {
        's'
    } else {
        'p'
    });
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

fn format_status(
    pid: usize,
    ppid: usize,
    comm: &str,
    memory_set: &MemorySet,
    real_uid: u32,
    effective_uid: u32,
    saved_uid: u32,
    real_gid: u32,
    effective_gid: u32,
    saved_gid: u32,
) -> String {
    let vm_size = memory_set.virtual_size_kb();
    let vm_rss = memory_set.resident_size_kb();
    format!(
        "Name:\t{}\n\
State:\tS (sleeping)\n\
Tgid:\t{}\n\
Pid:\t{}\n\
PPid:\t{}\n\
Uid:\t{}\t{}\t{}\t{}\n\
Gid:\t{}\t{}\t{}\t{}\n\
VmPeak:\t{:8} kB\n\
VmSize:\t{:8} kB\n\
VmHWM:\t{:8} kB\n\
VmRSS:\t{:8} kB\n\
VmSwap:\t       0 kB\n",
        comm,
        pid,
        pid,
        ppid,
        real_uid,
        effective_uid,
        saved_uid,
        effective_uid, // fs_uid defaults to effective_uid
        real_gid,
        effective_gid,
        saved_gid,
        effective_gid, // fs_gid defaults to effective_gid
        vm_size,
        vm_size,
        vm_rss,
        vm_rss,
    )
}

fn format_stat(
    pid: usize,
    ppid: usize,
    pgid: usize,
    state: char,
    comm: &str,
    memory_set: &MemorySet,
) -> String {
    let vsize = memory_set.virtual_size_kb() * 1024;
    let rss_pages = memory_set.resident_size_kb() * 1024 / PAGE_SIZE;
    format!(
        "{} ({}) {} {} {} 0 0 0 0 0 0 0 0 0 0 0 0 0 1 0 0 {} {} {} 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0\n",
        pid,
        comm,
        state,
        ppid,
        pgid,
        get_ticks(),
        vsize,
        rss_pages
    )
}

pub fn create_proc_dir_and_file(
    pid: usize,
    ppid: usize,
    pgid: usize,
    comm: &str,
    memory_set: &MemorySet,
    real_uid: u32,
    effective_uid: u32,
    saved_uid: u32,
    real_gid: u32,
    effective_gid: u32,
    saved_gid: u32,
) -> Result<(), SysErrNo> {
    register_proc_task(pid);
    let _procdir = open(
        format!("/proc/{}", pid).as_str(),
        OpenFlags::O_DIRECTORY | OpenFlags::O_CREATE | OpenFlags::O_RDWR,
        DEFAULT_DIR_MODE,
    )?
    .file()?;

    //创建进程状态文件/proc/<pid>/stat
    let statfile = open(
        format!("/proc/{}/stat", pid).as_str(),
        OpenFlags::O_CREATE | OpenFlags::O_RDWR | OpenFlags::O_TRUNC,
        DEFAULT_FILE_MODE,
    )
    .unwrap()
    .file()?;
    let mut statinfo = format_stat(pid, ppid, pgid, 'S', comm, memory_set);
    write_kernel_file(statfile.as_ref(), &mut statinfo)?;

    //创建进程状态文件/proc/<pid>/status
    let statusfile = open(
        format!("/proc/{}/status", pid).as_str(),
        OpenFlags::O_CREATE | OpenFlags::O_RDWR | OpenFlags::O_TRUNC,
        DEFAULT_FILE_MODE,
    )?
    .file()?;
    let mut statusinfo = format_status(
        pid,
        ppid,
        comm,
        memory_set,
        real_uid,
        effective_uid,
        saved_uid,
        real_gid,
        effective_gid,
        saved_gid,
    );
    write_kernel_file(statusfile.as_ref(), &mut statusinfo)?;

    // All entries share the same EXT4 write-back cache.  Delay the flush until
    // the complete proc directory is assembled so one fork performs one block
    // cache flush instead of one flush per file.
    refresh_proc_maps_unflushed(pid, memory_set)?;
    // Keep a zero-size directory entry for getdents-style proc enumeration.
    // Pagemap data itself is generated by PagemapFile when it is opened.
    let pagemapfile = open(
        format!("/proc/{}/pagemap", pid).as_str(),
        OpenFlags::O_CREATE | OpenFlags::O_RDWR,
        DEFAULT_FILE_MODE,
    )?
    .file()?;
    // Flush directory metadata and all four proc files once, after creation.
    pagemapfile.inode.sync();

    Ok(())
}

/// Create only the per-process proc directory.
///
/// Linux materializes task files when they are accessed.  Keep the directory
/// visible immediately, but defer stat/status/maps contents to the existing
/// refresh-on-open paths so fork does not scan VMAs or write four ext4 files
/// for processes that never inspect procfs.
pub fn create_proc_dir(pid: usize) -> Result<(), SysErrNo> {
    // Clone only publishes the process entry.  EXT4 creation is deferred until
    // `/proc/<pid>` is actually opened or `/proc` is enumerated.
    register_proc_task(pid);
    Ok(())
}

/// Rebuild `/proc/<pid>/maps` from the process's current VMA metadata.
///
/// Proc files are regular VFS files in Ya2yOS, so their contents must be
/// refreshed before every open instead of being treated as a creation-time
/// snapshot. Copy VMA metadata before entering the filesystem to avoid
/// holding the address-space lock across VFS operations.
pub fn refresh_proc_maps(pid: usize, memory_set: &MemorySet) -> Result<(), SysErrNo> {
    refresh_proc_maps_inner(pid, memory_set, true)
}

fn refresh_proc_maps_unflushed(pid: usize, memory_set: &MemorySet) -> Result<(), SysErrNo> {
    refresh_proc_maps_inner(pid, memory_set, false)
}

fn refresh_proc_maps_inner(
    pid: usize,
    memory_set: &MemorySet,
    flush: bool,
) -> Result<(), SysErrNo> {
    let areas = {
        let memory_set = memory_set.get_ref();
        memory_set
            .areas
            .iter()
            .map(|area| {
                (
                    area.vpn_range.start().0 * PAGE_SIZE,
                    area.vpn_range.end().0 * PAGE_SIZE,
                    area.map_perm,
                    area.mmap_flags,
                    area.mmap_file.offset,
                    area.mmap_file.file.clone(),
                )
            })
            .collect::<Vec<_>>()
    };

    let mapsfile = open(
        format!("/proc/{}/maps", pid).as_str(),
        OpenFlags::O_CREATE | OpenFlags::O_RDWR | OpenFlags::O_TRUNC,
        DEFAULT_FILE_MODE,
    )?
    .file()?;
    let mut mapsinfo = String::new();
    for (start, end, perm, flags, offset, file) in areas {
        let pathname = file.map(|file| file.inode.path()).unwrap_or_default();
        mapsinfo.push_str(&format!(
            "{:x}-{:x} {} {:08x} 00:00 0 {}\n",
            start,
            end,
            format_map_perm(perm, flags),
            offset,
            pathname
        ));
    }
    write_kernel_file(mapsfile.as_ref(), &mut mapsinfo)?;
    if flush {
        mapsfile.inode.sync();
    }
    Ok(())
}

pub fn refresh_proc_stat(
    pid: usize,
    ppid: usize,
    pgid: usize,
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
    let mut statinfo = format_stat(pid, ppid, pgid, state, comm, memory_set);
    write_kernel_file(statfile.as_ref(), &mut statinfo)?;
    statfile.inode.sync();
    Ok(())
}

pub fn refresh_proc_status(
    pid: usize,
    ppid: usize,
    comm: &str,
    memory_set: &MemorySet,
    real_uid: u32,
    effective_uid: u32,
    saved_uid: u32,
    real_gid: u32,
    effective_gid: u32,
    saved_gid: u32,
) -> Result<(), SysErrNo> {
    let statusfile = open(
        format!("/proc/{}/status", pid).as_str(),
        OpenFlags::O_CREATE | OpenFlags::O_RDWR | OpenFlags::O_TRUNC,
        DEFAULT_FILE_MODE,
    )?
    .file()?;
    let mut statusinfo = format_status(
        pid,
        ppid,
        comm,
        memory_set,
        real_uid,
        effective_uid,
        saved_uid,
        real_gid,
        effective_gid,
        saved_gid,
    );
    write_kernel_file(statusfile.as_ref(), &mut statusinfo)?;
    statusfile.inode.sync();
    Ok(())
}

pub fn remove_proc_dir_and_file(pid: usize) {
    unregister_proc_task(pid);
    for path in [
        format!("/proc/{}/pagemap", pid),
        format!("/proc/{}/maps", pid),
        format!("/proc/{}/status", pid),
        format!("/proc/{}/stat", pid),
    ] {
        if FsIndex::has_inode(&path) {
            superblock_root_inode().unlink(&path);
            FsIndex::remove_inode_idx(&path);
        }
    }
    let procdir = format!("/proc/{}", pid);
    if FsIndex::has_inode(&procdir) {
        superblock_root_inode().unlink(&procdir);
        FsIndex::remove_inode_idx(&procdir);
    }
}
