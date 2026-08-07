use super::Pipe;
use crate::fs::{FasyncOwner, File, Kstat, StMode};
use crate::mm::{copy_to_user, UserBuffer};
use crate::signal::{
    check_if_any_sig_for_current_task, consume_ignorable_pending_signal_for_current_task,
};
use crate::syscall::PollEvents;
use crate::task::{current_task, schedule_blocked_current, TaskStatus};
use crate::utils::{SysErrNo, SyscallRet};
use alloc::vec::Vec;
use core::cmp::min;
use core::sync::atomic::Ordering;
use core::task::Context;

const FIONREAD: u32 = 0x541B;
const IOC_WATCH_QUEUE_SET_SIZE: u32 = 0x5760;
const IOC_WATCH_QUEUE_SET_FILTER: u32 = 0x5761;

impl File for Pipe {
    fn readable(&self) -> bool {
        self.readable
    }

    fn writable(&self) -> bool {
        self.writable
    }

    fn read(&self, mut buf: UserBuffer) -> SyscallRet {
        assert!(self.readable());
        let requested = buf.len();
        #[cfg(feature = "perf")]
        crate::utils::perf::record_pipe_read_call(requested);
        let mut read_size = 0usize;
        loop {
            // Keep the availability check and consumption under one lock so
            // another reader cannot invalidate the byte count between them.
            let mut ring_buffer = self.inner_lock();
            let loop_read = ring_buffer.available_read();
            if loop_read == 0 {
                // 管道数据为空，需要进行阻塞或关闭
                if ring_buffer.all_write_ends_closed() {
                    // 写者全部关闭，不会有数据了，直接返回
                    drop(ring_buffer);
                    #[cfg(feature = "perf")]
                    crate::utils::perf::record_pipe_read_complete(requested, read_size);
                    return Ok(read_size);
                }
                if self.nonblocking() {
                    return Err(SysErrNo::EAGAIN);
                }
                drop(ring_buffer);
                if consume_ignorable_pending_signal_for_current_task() {
                    continue;
                }
                if check_if_any_sig_for_current_task().is_some() {
                    // 一旦获取信号，必须中断系统调用
                    return Err(SysErrNo::EINTR);
                }
                let task = current_task().ok_or(SysErrNo::ESRCH)?;
                // 与 Linux wait queue 一致：先发布睡眠态，再在同一条件锁下
                // 复查并登记 waiter；on_cpu 会阻止并发唤醒过早调度本上下文。
                let task_cx_ptr = {
                    let mut task_inner = task.inner_lock();
                    task_inner.task_status = TaskStatus::Blocked;
                    &mut task_inner.task_cx as *mut _
                };
                let mut ring_buffer = self.inner_lock();
                if ring_buffer.available_read() > 0 || ring_buffer.all_write_ends_closed() {
                    let mut task_inner = task.inner_lock();
                    task_inner.task_status = TaskStatus::Running;
                    continue;
                }
                ring_buffer.push_reader(&task);
                drop(ring_buffer);
                #[cfg(feature = "perf")]
                let wait_begin = crate::arch::time::get_ticks();
                schedule_blocked_current(task_cx_ptr);
                #[cfg(feature = "perf")]
                {
                    crate::utils::perf::record_pipe_read_wait_duration(
                        crate::arch::time::get_ticks().saturating_sub(wait_begin),
                    );
                    crate::utils::perf::record_pipe_read_wait_recheck();
                }
                continue;
            } else {
                // read at most loop_read bytes
                let length = requested;
                #[cfg(feature = "perf")]
                let copy_begin = crate::arch::time::get_ticks();
                if length <= 10 {
                    let mut buf_iter = buf.into_iter();
                    for _ in 0..loop_read {
                        if let Some(byte_ref) = buf_iter.next() {
                            unsafe {
                                *byte_ref = ring_buffer.read_byte();
                            }
                            read_size += 1;
                        } else {
                            break;
                        }
                    }
                } else {
                    read_size = min(loop_read, length);
                    #[cfg(feature = "perf")]
                    let user_copy_begin = crate::arch::time::get_ticks();
                    read_size = ring_buffer.read_into(&mut buf, read_size);
                    #[cfg(feature = "perf")]
                    crate::utils::perf::record_pipe_read_user_copy_duration(
                        crate::arch::time::get_ticks().saturating_sub(user_copy_begin),
                    );
                }
                #[cfg(feature = "perf")]
                let copy_elapsed = crate::arch::time::get_ticks().saturating_sub(copy_begin);
                ring_buffer.wake_writer();
                drop(ring_buffer);
                #[cfg(feature = "perf")]
                {
                    if read_size > 0 {
                        crate::utils::perf::record_pipe_read_copy_duration(copy_elapsed);
                    }
                    crate::utils::perf::record_pipe_read_complete(requested, read_size);
                }
                return Ok(read_size);
            }
        }
    }

    fn write(&self, buf: UserBuffer) -> SyscallRet {
        assert!(self.writable());
        let requested = buf.len();
        #[cfg(feature = "perf")]
        crate::utils::perf::record_pipe_write_call(requested);
        let mut write_size = 0usize;
        loop {
            // Keep the capacity check and write under one lock so another
            // writer cannot make the cached space count stale.
            let mut ring_buffer = self.inner_lock();
            if ring_buffer.all_read_ends_closed() {
                drop(ring_buffer);
                return Err(Self::broken_pipe());
            }
            let loop_write = ring_buffer.available_write();
            if loop_write == 0 {
                if self.nonblocking() {
                    return Err(SysErrNo::EAGAIN);
                }
                drop(ring_buffer);
                if consume_ignorable_pending_signal_for_current_task() {
                    continue;
                }
                if check_if_any_sig_for_current_task().is_some() {
                    return Err(SysErrNo::EINTR);
                }
                let task = current_task().ok_or(SysErrNo::ESRCH)?;
                // 先发布睡眠态，再在 pipe 锁下复查条件；并发唤醒只把状态
                // 改回 Ready，直到当前上下文完成切出后才允许重新入队。
                let task_cx_ptr = {
                    let mut task_inner = task.inner_lock();
                    task_inner.task_status = TaskStatus::Blocked;
                    &mut task_inner.task_cx as *mut _
                };
                let mut ring_buffer = self.inner_lock();
                if ring_buffer.all_read_ends_closed() {
                    let mut task_inner = task.inner_lock();
                    task_inner.task_status = TaskStatus::Running;
                    drop(task_inner);
                    drop(ring_buffer);
                    return Err(Self::broken_pipe());
                }
                if ring_buffer.available_write() > 0 {
                    let mut task_inner = task.inner_lock();
                    task_inner.task_status = TaskStatus::Running;
                    continue;
                }
                ring_buffer.push_writer(&task);
                drop(ring_buffer);
                #[cfg(feature = "perf")]
                let wait_begin = crate::arch::time::get_ticks();
                schedule_blocked_current(task_cx_ptr);
                #[cfg(feature = "perf")]
                {
                    crate::utils::perf::record_pipe_write_wait_duration(
                        crate::arch::time::get_ticks().saturating_sub(wait_begin),
                    );
                    crate::utils::perf::record_pipe_write_wait_recheck();
                }
                continue;
            } else {
                // write at most loop_write bytes
                let length = requested;
                #[cfg(feature = "perf")]
                let copy_begin = crate::arch::time::get_ticks();
                if length <= 10 {
                    let mut buf_iter = buf.into_iter();
                    for _ in 0..loop_write {
                        if let Some(byte_ref) = buf_iter.next() {
                            ring_buffer.write_byte(unsafe { *byte_ref });
                            write_size += 1;
                        } else {
                            break;
                        }
                    }
                } else {
                    write_size = min(loop_write, length);
                    #[cfg(feature = "perf")]
                    let user_extract_begin = crate::arch::time::get_ticks();
                    let mut bytes = Vec::with_capacity(write_size);
                    bytes.resize(write_size, 0);
                    let copied = buf.read_to(&mut bytes);
                    bytes.truncate(copied);
                    #[cfg(feature = "perf")]
                    crate::utils::perf::record_pipe_write_user_extract_duration(
                        crate::arch::time::get_ticks().saturating_sub(user_extract_begin),
                    );
                    #[cfg(feature = "perf")]
                    let pipebuf_copy_begin = crate::arch::time::get_ticks();
                    write_size = ring_buffer.write_owned_bytes(bytes);
                    #[cfg(feature = "perf")]
                    crate::utils::perf::record_pipe_write_pipebuf_copy_duration(
                        crate::arch::time::get_ticks().saturating_sub(pipebuf_copy_begin),
                    );
                }
                #[cfg(feature = "perf")]
                let copy_elapsed = crate::arch::time::get_ticks().saturating_sub(copy_begin);
                let async_owner = ring_buffer.async_owner;
                ring_buffer.wake_reader();
                drop(ring_buffer);
                #[cfg(feature = "perf")]
                {
                    if write_size > 0 {
                        crate::utils::perf::record_pipe_write_copy_duration(copy_elapsed);
                    }
                    crate::utils::perf::record_pipe_write_complete(requested, write_size);
                }
                if write_size > 0 {
                    self.notify_async_read_ready(async_owner);
                }
                return Ok(write_size);
            }
        }
    }

    fn write_kernel_bytes(&self, buf: &[u8]) -> SyscallRet {
        let mut ring_buffer = self.inner_lock();
        if ring_buffer.all_read_ends_closed() {
            drop(ring_buffer);
            return Err(Self::broken_pipe());
        }
        if ring_buffer.available_write() < buf.len() {
            return Err(SysErrNo::ENOBUFS);
        }
        ring_buffer.write_bytes(buf, buf.len());
        let async_owner = ring_buffer.async_owner;
        ring_buffer.wake_reader();
        drop(ring_buffer);
        if !buf.is_empty() {
            self.notify_async_read_ready(async_owner);
        }
        Ok(buf.len())
    }

    fn fstat(&self) -> Kstat {
        Kstat {
            st_mode: StMode::FIFO.bits(),
            st_nlink: 1,
            ..Kstat::default()
        }
    }

    fn nonblocking(&self) -> bool {
        self.nonblocking.load(Ordering::Relaxed)
    }

    fn set_nonblocking(&self, nonblocking: bool) -> Result<(), SysErrNo> {
        self.nonblocking.store(nonblocking, Ordering::Relaxed);
        Ok(())
    }

    fn set_fasync_owner(&self, owner: FasyncOwner) -> Result<(), SysErrNo> {
        self.set_async_owner(owner);
        Ok(())
    }

    fn fasync_owner(&self) -> FasyncOwner {
        self.async_owner()
    }

    fn ioctl(&self, cmd: u32, arg: usize, memory_set: &crate::mm::MemorySet) -> SyscallRet {
        match cmd {
            FIONREAD => {
                // Linux FIONREAD 返回当前可读字节数，类型为 int。
                let available = self.available_read() as i32;
                copy_to_user(memory_set, arg, &available.to_ne_bytes())?;
                Ok(0)
            }
            IOC_WATCH_QUEUE_SET_SIZE | IOC_WATCH_QUEUE_SET_FILTER => Ok(0),
            _ => Err(SysErrNo::ENOTTY),
        }
    }

    fn poll(&self, events: PollEvents) -> PollEvents {
        let mut revents = PollEvents::empty();
        let ring_buffer = self.inner_lock();
        if events.contains(PollEvents::IN) && self.readable && ring_buffer.available_read() > 0 {
            revents |= PollEvents::IN;
        }
        if events.contains(PollEvents::OUT) && self.writable && ring_buffer.available_write() > 0 {
            revents |= PollEvents::OUT;
        }
        if self.readable && ring_buffer.all_write_ends_closed() {
            revents |= PollEvents::HUP;
        }
        if self.writable && ring_buffer.all_read_ends_closed() {
            revents |= PollEvents::ERR;
        }
        revents
    }

    fn register(&self, context: &mut Context<'_>, events: PollEvents) {
        let ring_buffer = self.inner_lock();
        if self.readable && events.intersects(PollEvents::IN | PollEvents::HUP) {
            ring_buffer.read_poll.register(context.waker());
        }
        if self.writable && events.intersects(PollEvents::OUT | PollEvents::ERR) {
            ring_buffer.write_poll.register(context.waker());
        }
    }
}
