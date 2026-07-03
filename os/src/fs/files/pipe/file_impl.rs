use super::Pipe;
use crate::fs::{File, Kstat, StMode};
use crate::mm::{copy_to_user, UserBuffer};
use crate::signal::check_if_any_sig_for_current_task;
use crate::syscall::PollEvents;
use crate::task::{current_task, schedule_blocked_current, TaskStatus};
use crate::utils::{SysErrNo, SyscallRet};
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
        // let buf_len = buf.len();
        let mut read_size = 0usize;
        let mut loop_read;
        loop {
            let ring_buffer = self.inner_lock();
            loop_read = ring_buffer.available_read();
            if loop_read == 0 {
                // 管道数据为空，需要进行阻塞或关闭
                if ring_buffer.all_write_ends_closed() {
                    // 写者全部关闭，不会有数据了，直接返回
                    return Ok(read_size);
                }
                if self.nonblocking() {
                    return Err(SysErrNo::EAGAIN);
                }
                drop(ring_buffer);
                if check_if_any_sig_for_current_task().is_some() {
                    // 一旦获取信号，必须中断系统调用
                    return Err(SysErrNo::EINTR);
                }
                let task = current_task().ok_or(SysErrNo::ESRCH)?;
                // 先置 Blocked 再入队，避免写者并发唤醒时丢失 wakeup。
                let task_cx_ptr = {
                    let mut task_inner = task.inner_lock();
                    task_inner.task_status = TaskStatus::Blocked;
                    &mut task_inner.task_cx as *mut _
                };
                let mut ring_buffer = self.inner_lock();
                if ring_buffer.available_read() > 0 {
                    // 入队前已有写者写入，恢复 Running 并直接重试读取。
                    let mut task_inner = task.inner_lock();
                    task_inner.task_status = TaskStatus::Running;
                    continue;
                }
                ring_buffer.push_reader(&task);
                drop(ring_buffer);
                schedule_blocked_current(task_cx_ptr);
                continue;
            } else {
                break;
            }
        }
        // read at most loop_read bytes
        let mut ring_buffer = self.inner_lock();
        let length = buf.len();
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
            buf.write(&ring_buffer.read_bytes(read_size));
        }
        ring_buffer.wake_writer();
        Ok(read_size)
    }

    fn write(&self, mut buf: UserBuffer) -> SyscallRet {
        assert!(self.writable());
        let mut write_size = 0usize;
        let mut loop_write;
        loop {
            let ring_buffer = self.inner_lock();
            if ring_buffer.all_read_ends_closed() {
                drop(ring_buffer);
                return Err(Self::broken_pipe());
            }
            loop_write = ring_buffer.available_write();
            if loop_write == 0 {
                if self.nonblocking() {
                    return Err(SysErrNo::EAGAIN);
                }
                drop(ring_buffer);
                if check_if_any_sig_for_current_task().is_some() {
                    return Err(SysErrNo::EINTR);
                }
                let task = current_task().ok_or(SysErrNo::ESRCH)?;
                // 先置 Blocked 再入队，避免读者并发唤醒时丢失 wakeup。
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
                    // 入队前已有读者释放空间，恢复 Running 并直接重试写入。
                    let mut task_inner = task.inner_lock();
                    task_inner.task_status = TaskStatus::Running;
                    continue;
                }
                ring_buffer.push_writer(&task);
                drop(ring_buffer);
                schedule_blocked_current(task_cx_ptr);
                continue;
            } else {
                break;
            }
        }
        // write at most loop_write bytes
        let mut ring_buffer = self.inner_lock();
        let length = buf.len();
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
            ring_buffer.write_bytes(&buf.read(write_size), write_size);
        }
        ring_buffer.wake_reader();
        Ok(write_size)
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
        ring_buffer.wake_reader();
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
