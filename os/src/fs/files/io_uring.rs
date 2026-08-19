//! `io_uring` 实例的文件对象和用户可见参数布局。
//!
//! 当前版本只完成 `io_uring_setup` 所需的 ABI 结构与独立 fd 类型，尚未实现
//! submission queue、completion queue 及异步操作执行引擎。因此该 fd 不模拟
//! 普通文件：不能直接读写，也不会报告可读/可写事件。保留独立类型是为了让
//! 后续实现可以在不改变 fd 识别方式的前提下接入真正的 ring 状态。
//!
//! The ring submission/completion engine is not implemented yet, but keeping a
//! distinct file type prevents `io_uring_setup` from returning an unrelated
//! dummy descriptor.
use alloc::sync::Arc;

use super::super::{File, Kstat};
use crate::mm::UserBuffer;
use crate::syscall::PollEvents;
use crate::utils::{SysErrNo, SyscallRet};

pub const IORING_MAX_ENTRIES: u32 = 32_768;

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct IoSqringOffsets {
    /// 用户态 SQ head 的共享内存偏移。
    pub head: u32,
    /// 用户态 SQ tail 的共享内存偏移。
    pub tail: u32,
    /// SQ 环形索引掩码的偏移。
    pub ring_mask: u32,
    /// SQ 项数的偏移。
    pub ring_entries: u32,
    /// SQ flags 的偏移。
    pub flags: u32,
    /// 被内核丢弃的 SQE 计数偏移。
    pub dropped: u32,
    /// SQ 间接索引数组偏移。
    pub array: u32,
    /// 保留字段。
    pub resv1: u32,
    /// 保留字段，保持 Linux ABI 对齐。
    pub resv2: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct IoCqringOffsets {
    /// 用户态 CQ head 的共享内存偏移。
    pub head: u32,
    /// 内核 CQ tail 的共享内存偏移。
    pub tail: u32,
    /// CQ 环形索引掩码的偏移。
    pub ring_mask: u32,
    /// CQ 项数的偏移。
    pub ring_entries: u32,
    /// CQ 溢出计数偏移。
    pub overflow: u32,
    /// CQE 数组偏移。
    pub cqes: u32,
    /// CQ flags 的偏移。
    pub flags: u32,
    /// 保留字段。
    pub resv1: u32,
    /// 保留字段，保持 Linux ABI 对齐。
    pub resv2: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct IoUringParams {
    pub sq_entries: u32,
    pub cq_entries: u32,
    pub flags: u32,
    pub sq_thread_cpu: u32,
    pub sq_thread_idle: u32,
    pub features: u32,
    pub wq_fd: u32,
    pub resv: [u32; 3],
    pub sq_off: IoSqringOffsets,
    pub cq_off: IoCqringOffsets,
}

/// `io_uring_setup` 返回的占位文件对象。
///
/// 该类型当前没有内部字段，因为 ring 内存和操作队列尚未实现；它只用于让
/// fd 表能够区分 io_uring 描述符，并为未来扩展保留 `File` trait 入口。
pub struct IoUringFd;

impl IoUringFd {
    pub fn new() -> Arc<Self> {
        Arc::new(Self)
    }
}

impl File for IoUringFd {
    fn readable(&self) -> bool {
        false
    }

    fn writable(&self) -> bool {
        false
    }

    fn read(&self, _buf: UserBuffer) -> SyscallRet {
        Err(SysErrNo::EINVAL)
    }

    fn write(&self, _buf: UserBuffer) -> SyscallRet {
        Err(SysErrNo::EINVAL)
    }

    fn fstat(&self) -> Kstat {
        Kstat::default()
    }

    fn poll(&self, _events: PollEvents) -> PollEvents {
        PollEvents::empty()
    }
}
