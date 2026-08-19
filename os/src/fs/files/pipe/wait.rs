//! 管道阻塞等待、唤醒竞态和双缓冲区加锁顺序。
//!
//! 读写等待都采用“先发布阻塞状态，再在缓冲区锁下复查条件并登记 waiter”
//! 的模式，覆盖检查与入队之间的唤醒竞态。跨管道操作使用地址排序获取两把
//! 锁，从而保证 `splice` 与 `tee` 并发时不会因锁顺序不同而死锁。

use super::ring_buffer::PipeRingBuffer;
use super::Pipe;
use crate::fs::File;
use crate::signal::{
    check_if_any_sig_for_current_task, consume_ignorable_pending_signal_for_current_task,
};
use crate::task::{current_task, schedule_blocked_current, TaskStatus};
use crate::utils::SysErrNo;
use alloc::sync::Arc;

impl Pipe {
    /// 等待 pipe 变为可读。阻塞前先把当前任务置为 Blocked，再重新检查条件并入队，
    /// 这样可以覆盖“解锁后、入队前”写者唤醒造成的竞态窗口。
    pub(super) fn wait_readable(&self, nonblock: bool) -> Result<(), SysErrNo> {
        loop {
            let ring_buffer = self.inner_lock();
            if ring_buffer.available_read() > 0 || ring_buffer.all_write_ends_closed() {
                return Ok(());
            }
            if nonblock || self.nonblocking() {
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
            drop(task);
            schedule_blocked_current(task_cx_ptr);
        }
    }

    /// 等待 pipe 变为可写。逻辑与 wait_readable 对称，同时需要处理所有读端关闭时的 EPIPE。
    pub(super) fn wait_writable(&self, nonblock: bool) -> Result<(), SysErrNo> {
        loop {
            let ring_buffer = self.inner_lock();
            if ring_buffer.all_read_ends_closed() {
                return Err(SysErrNo::EPIPE);
            }
            if ring_buffer.available_write() > 0 {
                return Ok(());
            }
            if nonblock || self.nonblocking() {
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
            let task_cx_ptr = {
                let mut task_inner = task.inner_lock();
                task_inner.task_status = TaskStatus::Blocked;
                &mut task_inner.task_cx as *mut _
            };
            let mut ring_buffer = self.inner_lock();
            if ring_buffer.all_read_ends_closed() {
                let mut task_inner = task.inner_lock();
                task_inner.task_status = TaskStatus::Running;
                return Err(SysErrNo::EPIPE);
            }
            if ring_buffer.available_write() > 0 {
                let mut task_inner = task.inner_lock();
                task_inner.task_status = TaskStatus::Running;
                continue;
            }
            ring_buffer.push_writer(&task);
            drop(ring_buffer);
            drop(task);
            schedule_blocked_current(task_cx_ptr);
        }
    }

    /// 同时锁两个 pipe buffer 时按 Arc 地址排序，保证所有调用点使用一致锁顺序。
    /// splice/tee 需要同时观察输入和输出 pipe，固定锁顺序可以避免两个方向并发操作时死锁。
    pub(super) fn with_ordered_buffers<T>(
        &self,
        other: &Pipe,
        f: impl FnOnce(&mut PipeRingBuffer, &mut PipeRingBuffer) -> Result<T, SysErrNo>,
    ) -> Result<T, SysErrNo> {
        let self_addr = Arc::as_ptr(&self.buffer) as usize;
        let other_addr = Arc::as_ptr(&other.buffer) as usize;
        if self_addr < other_addr {
            let mut first = self.inner_lock();
            let mut second = other.inner_lock();
            f(&mut first, &mut second)
        } else {
            let mut first = other.inner_lock();
            let mut second = self.inner_lock();
            f(&mut second, &mut first)
        }
    }
}
