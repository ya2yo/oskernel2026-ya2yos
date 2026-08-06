//! 进程记账（process accounting）实现。
//!
//! 由 `acct(2)` 系统调用开启：`sys_acct` 校验权限与目标文件后，通过
//! [`set_process_acct_file`] 把记账文件注册到内核；此后每个进程退出时，
//! 由 [`write_process_acct_record`] 以 Linux 旧版 `struct acct` 格式追加一条
//! 记账记录。

use crate::{
    fs::{File, OSFile, SEEK_END},
    task::{ProcessUsage, TaskControlBlock},
};
use alloc::{sync::Arc, vec::Vec};
use core::mem::size_of;
use spin::{Lazy, Mutex};

use log::warn;

/// 全局记账文件句柄。`Some(file)` 表示记账已开启，所有进程退出时都会向该文件
/// 追加记录；`None` 表示未开启（或已由 `acct(NULL)` 关闭）。
static ACCT_FILE: Lazy<Mutex<Option<Arc<OSFile>>>> = Lazy::new(|| Mutex::new(None));

/// 设置或关闭全局记账文件，供 `sys_acct` 调用。
///
/// 传 `None` 表示关闭记账；传 `Some(file)` 表示开启记账。
pub(crate) fn set_process_acct_file(file: Option<Arc<OSFile>>) {
    *ACCT_FILE.lock() = file;
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

/// 将计数编码为 Linux `comp_t`（13 位尾数 + 3 位 2^3 指数）。
///
/// 与内核 `encode_comp_t` 的舍入规则一致：每次右移 3 位前先加 7（四舍五入），
/// 指数上限为 7，超出部分截断。
fn encode_comp_t(mut value: u64) -> u16 {
    let mut exp = 0u16;
    while value > 0x1fff && exp < 7 {
        value = (value + 7) >> 3;
        exp += 1;
    }
    ((exp & 0x7) << 13) | (value as u16 & 0x1fff)
}

/// 将毫秒时间换算为用户态时钟 tick（HZ = 100），供 ac_utime/ac_stime/ac_etime 使用。
///
/// 非正值（含出错产生的负值）一律按 0 处理。
fn ms_to_user_ticks(ms: isize) -> u64 {
    if ms <= 0 {
        0
    } else {
        (ms as u64).saturating_mul(100) / 1000
    }
}

/// 由退出码和终止信号构造 `ac_exitcode`（即 wait 状态字）。
///
/// 被信号终止时返回 `signo | (core_dumped ? 0x80 : 0)`，与 Linux 的
/// `W_EXITCODE`/`W_STOPCODE` 语义一致；正常退出时返回 `exit_code << 8`。
fn wait_status_from_exit_code(exit_code: i32, termination_signal: Option<(usize, bool)>) -> u32 {
    if let Some((signo, dumped_core)) = termination_signal {
        signo as u32 | if dumped_core { 0x80 } else { 0 }
    } else {
        (exit_code as u32) << 8
    }
}

/// 从任务及其累计用量构建一条 [`AcctRecord`]。
///
/// 读取 uid/gid 时需要持任务锁，读取 comm 与终止信号时需持进程元数据锁；
/// 两处锁按任务锁在前、元数据锁在后的顺序获取，避免与进程退出路径死锁。
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
        ac_btime: crate::timer::realtime().tv_sec as u32,
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

/// 若记账已开启，将当前进程的记账记录以追加方式写入记账文件并落盘。
///
/// 在进程退出路径（`exit` 回收阶段）调用；记账未开启时直接返回。
/// 写入失败只记录告警日志，不影响进程退出流程。
pub(crate) fn write_process_acct_record(
    task: &TaskControlBlock,
    exit_code: i32,
    usage: &ProcessUsage,
) {
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
