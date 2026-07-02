use crate::{
    fs::{open, File, FileClass, InodeType, OpenFlags, MAX_PATH_LEN, NONE_MODE, SEEK_END},
    mm::{if_bad_address, read_user_cstr},
    task::{current_task, ProcessUsage, TaskControlBlock},
    timer::realtime,
    utils::{get_abs_path, SysErrNo, SyscallRet},
};
use alloc::{sync::Arc, vec::Vec};
use core::mem::size_of;
use spin::{Lazy, Mutex};

use log::{debug, warn};

/// 全局进程记账文件
/// acct(2) 系统调用用于开启/关闭进程记账。
/// 开启时把生成的记账记录写入指定文件；关闭时传入 NULL。
static ACCT_FILE: Lazy<Mutex<Option<Arc<crate::fs::OSFile>>>> = Lazy::new(|| Mutex::new(None));

/// 参考 https://man7.org/linux/man-pages/man2/acct.2.html
pub fn sys_acct(filename: *const u8) -> SyscallRet {
    let task = current_task().unwrap();
    let task_inner = task.inner_lock();
    let uid = task_inner.user_id;
    drop(task_inner);

    // NULL 表示关闭记账
    if filename.is_null() {
        // 非 root 用户无权关闭记账
        if uid != 0 {
            return Err(SysErrNo::EPERM);
        }
        *ACCT_FILE.lock() = None;
        debug!("[sys_acct] accounting disabled");
        return Ok(0);
    }

    // 只有 root 用户可以开启记账
    if uid != 0 {
        return Err(SysErrNo::EPERM);
    }

    // 检查用户空间地址合法性
    if (filename as isize) <= 0 || if_bad_address(filename as usize) {
        return Err(SysErrNo::EFAULT);
    }

    let proc_inner = &task.process;
    let memory_set = proc_inner.get_locked_memory_set_read();
    let path = read_user_cstr(&memory_set, filename)?;

    // 检查路径长度
    if path.len() > MAX_PATH_LEN {
        return Err(SysErrNo::ENAMETOOLONG);
    }

    // 计算绝对路径
    let abs_path = get_abs_path(&proc_inner.fs_info.get_cwd(), &path);
    debug!("[sys_acct] filename = {}, abs_path = {}", path, abs_path);
    drop(memory_set);

    // 尝试打开文件以验证路径有效
    let file_class = open(&abs_path, OpenFlags::O_WRONLY, NONE_MODE)?;

    // 必须为普通文件，不能为设备、socket 等
    let osfile = match &file_class {
        FileClass::File(f) => f.clone(),
        _ => {
            // 非普通文件（设备、socket 等）返回 EACCES
            return Err(SysErrNo::EACCES);
        }
    };

    // 检查是否为目录
    if osfile.inode.types().is_dir() {
        return Err(SysErrNo::EISDIR);
    }

    // 检查是否为普通文件
    if !osfile.inode.types().is_file() {
        return Err(SysErrNo::EACCES);
    }
    // 存储记账文件
    *ACCT_FILE.lock() = Some(osfile);
    debug!("[sys_acct] accounting enabled, file = {}", abs_path);
    Ok(0)
}

/// Linux 旧版 process accounting 记录格式，对应 LTP lapi/acct.h 的 struct acct。
/// 显式保留 C ABI padding，避免把 Rust 未初始化 padding 字节写入文件。
#[repr(C)]
#[derive(Clone, Copy)]
struct AcctRecord {
    ac_flag: u8,
    _pad_after_flag: u8,
    ac_uid: u16,
    ac_gid: u16,
    ac_tty: u16,
    ac_btime: u32,
    ac_utime: u16,
    ac_stime: u16,
    ac_etime: u16,
    ac_mem: u16,
    ac_io: u16,
    ac_rw: u16,
    ac_minflt: u16,
    ac_majflt: u16,
    ac_swaps: u16,
    _pad_before_exitcode: u16,
    ac_exitcode: u32,
    ac_comm: [u8; 17],
    ac_pad: [u8; 10],
    _final_pad: u8,
}

const _: () = assert!(size_of::<AcctRecord>() == 64);

fn encode_comp_t(mut value: u64) -> u16 {
    let mut exp = 0u16;
    while value > 0x1fff && exp < 7 {
        value = (value + 7) >> 3;
        exp += 1;
    }
    ((exp & 0x7) << 13) | (value as u16 & 0x1fff)
}

fn ms_to_user_ticks(ms: isize) -> u64 {
    if ms <= 0 {
        0
    } else {
        (ms as u64).saturating_mul(100) / 1000
    }
}

fn wait_status_from_exit_code(exit_code: i32, termination_signal: Option<(usize, bool)>) -> u32 {
    if let Some((signo, dumped_core)) = termination_signal {
        signo as u32 | if dumped_core { 0x80 } else { 0 }
    } else if exit_code >= 128 && exit_code <= 255 {
        // 当前 wait4 路径会把 128..255 当作已编码状态返回；
        // acct02 也按用户态可见的 WEXITSTATUS(status) << 8 比对。
        0
    } else {
        (exit_code as u32) << 8
    }
}

fn build_acct_record(task: &TaskControlBlock, exit_code: i32, usage: &ProcessUsage) -> AcctRecord {
    let task_inner = task.inner_lock();
    let uid = task_inner.user_id as u16;
    let gid = task_inner.real_gid as u16;
    drop(task_inner);

    let (comm, termination_signal) = {
        let meta = task.process.meta_lock();
        (meta.comm.clone(), meta.termination_signal)
    };
    let mut ac_comm = [0u8; 17];
    for (dst, src) in ac_comm.iter_mut().take(16).zip(comm.as_bytes().iter()) {
        *dst = *src;
    }

    let user_ticks = ms_to_user_ticks(usage.utime);
    let sys_ticks = ms_to_user_ticks(usage.stime);
    let elapsed_ticks = user_ticks.saturating_add(sys_ticks);

    AcctRecord {
        ac_flag: 0,
        _pad_after_flag: 0,
        ac_uid: uid,
        ac_gid: gid,
        ac_tty: 0,
        ac_btime: realtime().tv_sec as u32,
        ac_utime: encode_comp_t(user_ticks),
        ac_stime: encode_comp_t(sys_ticks),
        ac_etime: encode_comp_t(elapsed_ticks),
        ac_mem: 0,
        ac_io: 0,
        ac_rw: 0,
        ac_minflt: 0,
        ac_majflt: 0,
        ac_swaps: 0,
        _pad_before_exitcode: 0,
        ac_exitcode: wait_status_from_exit_code(exit_code, termination_signal),
        ac_comm,
        ac_pad: [0; 10],
        _final_pad: 0,
    }
}

pub fn write_process_acct_record(task: &TaskControlBlock, exit_code: i32, usage: &ProcessUsage) {
    let acct_file = ACCT_FILE.lock().clone();
    let Some(file) = acct_file else {
        return;
    };

    let mut record = build_acct_record(task, exit_code, usage);
    let mut buffers = Vec::new();
    unsafe {
        buffers.push(core::slice::from_raw_parts_mut(
            &mut record as *mut AcctRecord as *mut u8,
            size_of::<AcctRecord>(),
        ));
    }

    if let Err(err) = file
        .lseek(0, SEEK_END)
        .and_then(|_| file.write(crate::mm::UserBuffer::new(buffers)))
    {
        warn!("[acct] failed to write accounting record: {:?}", err);
        return;
    }
    file.inode.sync();
}
