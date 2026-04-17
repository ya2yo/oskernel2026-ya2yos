use crate::{arch::time::get_ticks, mm::UserBuffer};

// 该文件创建形如/proc/xxx的文件
use super::*;
use alloc::{format, vec::Vec};
pub fn create_proc_dir_and_file(pid: usize, ppid: usize) -> Result<(), SysErrNo> {
    open(
        format!("/proc/{}", pid).as_str(),
        OpenFlags::O_DIRECTORY | OpenFlags::O_CREATE | OpenFlags::O_RDWR,
        DEFAULT_DIR_MODE,
    )
    .unwrap()
    .file()?;

    //创建进程状态文件/proc/<pid>/stat
    let statfile = open(
        format!("/proc/{}/stat", pid).as_str(),
        OpenFlags::O_CREATE | OpenFlags::O_RDWR,
        DEFAULT_FILE_MODE,
    )
    .unwrap()
    .file()?;
    let mut statinfo = format!(
        "{} (busybox) S {} 0 0 0 0 0 0 0 0 0 {} 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0",
        pid,
        ppid,
        get_ticks()
    );
    let mut statvec = Vec::new();
    unsafe {
        let stat = statinfo.as_bytes_mut();
        statvec.push(core::slice::from_raw_parts_mut(
            stat.as_mut_ptr(),
            stat.len(),
        ));
    }
    let statbuf = UserBuffer::new(statvec);
    statfile.write(statbuf)?;
    // debug!("create /proc/{}/stat with {} sizes", pid, statsize);
    Ok(())
}

pub fn remove_proc_dir_and_file(pid: usize) {
    superblock_root_inode().unlink(format!("/proc/{}/stat", pid).as_str());
    FsIndex::remove_inode_idx(format!("/proc/{}/stat", pid).as_str());
    superblock_root_inode().unlink(format!("/proc/{}", pid).as_str());
    FsIndex::remove_inode_idx(format!("/proc/{}", pid).as_str());
}
