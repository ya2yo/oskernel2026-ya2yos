//! 该模块实现匿名 pipe 和 FIFO 端点。
//! Pipe 对外表现为 File trait，内部用按字节计数的 PipeBuf 片段队列保存数据。
//! 普通 write 会拷贝用户数据生成 Bytes 片段；splice/tee 和 file page cache 路径可以通过
//! 移动或克隆 PipeBuf 引用来减少跨 pipe 复制。

mod buffer;
mod fifo;
mod file_impl;
mod ring_buffer;
mod splice;
mod wait;

pub use fifo::open_fifo;

use self::ring_buffer::PipeRingBuffer;
use crate::arch::memory_layout::PAGE_SIZE;
use crate::fs::{FasyncOwner, File};
use crate::signal::{
    send_signal_to_process_group, send_signal_to_thread, send_signal_to_thread_group, SigSet, SIGIO,
};
use crate::task::current_task;
use crate::utils::{page_round_up, SysErrNo};
use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, Ordering};
use spin::{Mutex, MutexGuard};

pub const PIPE_DEFAULT_SIZE: usize = 65536;
pub const PIPE_MAX_SIZE: usize = 65536;

pub struct Pipe {
    readable: bool,
    writable: bool,
    nonblocking: AtomicBool,
    buffer: Arc<Mutex<PipeRingBuffer>>,
}

impl Pipe {
    fn inner_lock(&self) -> MutexGuard<'_, PipeRingBuffer> {
        self.buffer.lock()
    }

    /// 创建管道的读端
    fn read_end_with_buffer(buffer: Arc<Mutex<PipeRingBuffer>>) -> Self {
        Self::with_buffer(true, false, buffer)
    }

    /// 创建管道的写端
    fn write_end_with_buffer(buffer: Arc<Mutex<PipeRingBuffer>>) -> Self {
        Self::with_buffer(false, true, buffer)
    }

    fn with_buffer(readable: bool, writable: bool, buffer: Arc<Mutex<PipeRingBuffer>>) -> Self {
        let pipe = Self {
            readable,
            writable,
            nonblocking: AtomicBool::new(false),
            buffer,
        };
        {
            let mut ring_buffer = pipe.inner_lock();
            if readable {
                ring_buffer.read_end_count += 1;
            }
            if writable {
                ring_buffer.write_end_count += 1;
            }
        }
        pipe
    }

    fn fifo_end_with_buffer(
        buffer: Arc<Mutex<PipeRingBuffer>>,
        flags: crate::fs::OpenFlags,
    ) -> Result<Self, SysErrNo> {
        let (readable, writable) = flags.read_write();
        if !readable && !writable {
            return Err(SysErrNo::EINVAL);
        }
        let pipe = Self::with_buffer(readable, writable, buffer);
        if flags.contains(crate::fs::OpenFlags::O_NONBLOCK) {
            pipe.set_nonblocking(true)?;
        }
        Ok(pipe)
    }

    /// 创建同时可读写的 FIFO 端
    #[allow(dead_code)]
    fn read_write_end_with_buffer(buffer: Arc<Mutex<PipeRingBuffer>>) -> Self {
        Self::with_buffer(true, true, buffer)
    }

    /// 获取管道中剩余可读长度
    /// 该函数的意义是套壳，将PipeRingBuffer对外隐藏起来
    pub fn available_read(&self) -> usize {
        return self.inner_lock().available_read();
    }

    /// 获取管道中剩余可写长度
    /// 该函数的意义是套壳，将PipeRingBuffer对外隐藏起来
    pub fn available_write(&self) -> usize {
        return self.inner_lock().available_write();
    }

    pub fn capacity(&self) -> usize {
        self.inner_lock().capacity()
    }

    pub fn set_capacity(&self, requested: usize) -> Result<usize, SysErrNo> {
        let capacity = if requested == 0 {
            PAGE_SIZE
        } else {
            page_round_up(requested)
        };
        if capacity > PIPE_MAX_SIZE {
            return Err(SysErrNo::EPERM);
        }

        let mut ring_buffer = self.inner_lock();
        if capacity < ring_buffer.available_read() {
            return Err(SysErrNo::EBUSY);
        }
        ring_buffer.set_capacity(capacity);
        ring_buffer.wake_all_writers();
        Ok(capacity)
    }

    pub fn all_read_ends_closed(&self) -> bool {
        self.inner_lock().all_read_ends_closed()
    }

    pub fn all_write_ends_closed(&self) -> bool {
        self.inner_lock().all_write_ends_closed()
    }

    pub(super) fn set_async_owner(&self, owner: FasyncOwner) {
        self.inner_lock().async_owner = owner;
    }

    pub(super) fn async_owner(&self) -> FasyncOwner {
        self.inner_lock().async_owner
    }

    pub(super) fn notify_async_read_ready(&self, owner: FasyncOwner) {
        if owner.pid <= 0 {
            return;
        }
        let signo = if owner.signal == 0 {
            SIGIO
        } else {
            owner.signal as usize
        };
        let sig = SigSet::from_sig(signo);
        match owner.owner_type {
            0 => send_signal_to_thread(owner.pid as usize, sig),
            1 => {
                let _ = send_signal_to_thread_group(owner.pid as usize, sig);
            }
            2 => send_signal_to_process_group(owner.pid as usize, sig),
            _ => {}
        }
    }

    /// Linux pipe 写入无读端时需要同时发送 SIGPIPE，并把 syscall 结果报告为 EPIPE。
    /// 调用该函数前不要持有 pipe/task 锁，避免 signal 路径和调度路径形成锁嵌套。
    fn broken_pipe() -> SysErrNo {
        if let Some(task) = current_task() {
            send_signal_to_thread(task.tid(), SigSet::SIGPIPE);
        }
        SysErrNo::EPIPE
    }
}

/// 创建一个管道并返回管道的读端和写端 (read_end, write_end)
pub fn make_pipe() -> (Arc<Pipe>, Arc<Pipe>) {
    let buffer = Arc::new(Mutex::new(PipeRingBuffer::new()));
    let read_end = Arc::new(Pipe::read_end_with_buffer(buffer.clone()));
    let write_end = Arc::new(Pipe::write_end_with_buffer(buffer.clone()));
    (read_end, write_end)
}

impl Drop for Pipe {
    fn drop(&mut self) {
        let mut ring_buffer = self.inner_lock();
        if self.readable {
            ring_buffer.read_end_count = ring_buffer.read_end_count.saturating_sub(1);
            ring_buffer.wake_all_writers();
        }
        if self.writable {
            ring_buffer.write_end_count = ring_buffer.write_end_count.saturating_sub(1);
            ring_buffer.wake_all_readers();
        }
    }
}
