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
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use linux_raw_sys::general::CAP_SYS_RESOURCE;
use spin::{Mutex, MutexGuard};

pub const PIPE_DEFAULT_SIZE: usize = 65536;
pub const PIPE_MAX_SIZE: usize = 65536;

static PIPE_MAX_SIZE_SYSCTL: AtomicUsize = AtomicUsize::new(PIPE_MAX_SIZE);

fn has_cap_sys_resource() -> bool {
    current_task().map_or(false, |task| {
        let inner = task.inner_lock();
        let cap = CAP_SYS_RESOURCE as usize;
        let word = cap / 32;
        let bit = cap % 32;
        word < inner.capabilities.effective.len()
            && (inner.capabilities.effective[word] & (1u32 << bit)) != 0
    })
}

/// 返回新建管道默认遵循的系统级容量上限。
pub fn pipe_max_size() -> usize {
    PIPE_MAX_SIZE_SYSCTL.load(Ordering::Relaxed)
}

/// 设置非特权进程可使用的管道容量上限，并按页大小向上取整。
///
/// 取值必须位于一个页到编译期最大值之间；拥有 `CAP_SYS_RESOURCE` 的任务
/// 不受该运行时上限限制，但仍不能超过编译期最大值。
pub fn set_pipe_max_size(value: usize) -> Result<(), SysErrNo> {
    if value < PAGE_SIZE || value > PIPE_MAX_SIZE {
        return Err(SysErrNo::EINVAL);
    }
    PIPE_MAX_SIZE_SYSCTL.store(page_round_up(value), Ordering::Relaxed);
    Ok(())
}

/// 管道的一个文件端点。
///
/// 读端和写端共享同一个 [`PipeRingBuffer`]，但通过 `readable`/`writable`
/// 限制各自能力；端点销毁时更新共享计数，以便另一侧观察 EOF 或 EPIPE。
pub struct Pipe {
    /// 当前端点是否允许读取。
    readable: bool,
    /// 当前端点是否允许写入。
    writable: bool,
    /// 当前端点的非阻塞属性。
    nonblocking: AtomicBool,
    /// 读写端之间共享的缓冲区和等待队列。
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

    /// 获取管道中当前可读取的字节数。
    ///
    /// 该方法只负责在内部锁外提供缓冲区计数的安全访问。
    pub fn available_read(&self) -> usize {
        return self.inner_lock().available_read();
    }

    /// 获取管道当前还可容纳的字节数。
    ///
    /// 返回值由容量减去已缓存字节数得到，调用期间由内部锁保护。
    pub fn available_write(&self) -> usize {
        return self.inner_lock().available_write();
    }

    /// 返回管道当前容量（字节）。
    pub fn capacity(&self) -> usize {
        self.inner_lock().capacity()
    }

    /// 调整管道容量并返回实际采用的、按页对齐后的大小。
    ///
    /// 缩容不能小于当前已缓存数据；非特权任务还受到运行时容量上限约束。
    pub fn set_capacity(&self, requested: usize) -> Result<usize, SysErrNo> {
        let capacity = if requested == 0 {
            PAGE_SIZE
        } else {
            page_round_up(requested)
        };
        if capacity > PIPE_MAX_SIZE || (!has_cap_sys_resource() && capacity > pipe_max_size()) {
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

    /// 判断共享缓冲区是否已经没有任何读端。
    pub fn all_read_ends_closed(&self) -> bool {
        self.inner_lock().all_read_ends_closed()
    }

    /// 判断共享缓冲区是否已经没有任何写端。
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

/// 创建一个管道并返回管道的读端和写端 `(read_end, write_end)`。
///
/// 初始容量取默认值与当前系统上限的较小者；两个端点随后共享同一个环形
/// 片段队列，并分别增加读端、写端引用计数。
pub fn make_pipe() -> (Arc<Pipe>, Arc<Pipe>) {
    let capacity = if has_cap_sys_resource() {
        PIPE_DEFAULT_SIZE
    } else {
        PIPE_DEFAULT_SIZE.min(pipe_max_size())
    };
    let buffer = Arc::new(Mutex::new(PipeRingBuffer::with_capacity(capacity)));
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
