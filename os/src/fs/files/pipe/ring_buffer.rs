use super::buffer::PipeBuf;
use crate::task::{TaskControlBlock, TaskStatus, ready_queue};
use crate::utils::PollSet;
use alloc::collections::VecDeque;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;

const RING_BUFFER_SIZE: usize = 65536;

/// Pipe 的内层共享缓冲区。
/// 名称保留 RingBuffer，但实际已经是按字节容量限制的 PipeBuf 片段队列。
pub(super) struct PipeRingBuffer {
    bufs: VecDeque<PipeBuf>,
    bytes: usize,
    pub(super) write_end_count: usize,
    pub(super) read_end_count: usize,
    read_waiters: VecDeque<Weak<TaskControlBlock>>,
    write_waiters: VecDeque<Weak<TaskControlBlock>>,
    pub(super) read_poll: PollSet,
    pub(super) write_poll: PollSet,
}

impl PipeRingBuffer {
    pub(super) fn new() -> Self {
        Self {
            bufs: VecDeque::new(),
            bytes: 0,
            write_end_count: 0,
            read_end_count: 0,
            read_waiters: VecDeque::new(),
            write_waiters: VecDeque::new(),
            read_poll: PollSet::new(),
            write_poll: PollSet::new(),
        }
    }

    /// 写一个字节到管道尾
    pub(super) fn write_byte(&mut self, byte: u8) {
        self.write_bytes(&[byte], 1);
    }

    /// 写n个字节到管道尾
    pub(super) fn write_bytes(&mut self, bytes: &[u8], len: usize) {
        assert!(len <= self.available_write(), "pipe buffer overflow");
        let len = len.min(bytes.len());
        if len == 0 {
            return;
        }
        self.bufs.push_back(PipeBuf::new(bytes[..len].to_vec()));
        self.bytes += len;
    }

    /// 从管道头读一个字节
    pub(super) fn read_byte(&mut self) -> u8 {
        self.read_bytes(1)[0]
    }

    /// 从管道头读n个字节
    pub(super) fn read_bytes(&mut self, len: usize) -> Vec<u8> {
        let len = len.min(self.available_read());
        let mut bytes = Vec::with_capacity(len);
        for buf in self.pop_bufs(len) {
            bytes.extend_from_slice(buf.as_slice());
        }
        bytes
    }

    /// 获取管道中剩余可读长度
    pub(super) fn available_read(&self) -> usize {
        self.bytes
    }

    /// 获取管道中剩余可写长度
    pub(super) fn available_write(&self) -> usize {
        RING_BUFFER_SIZE - self.bytes
    }

    /// 追加一组片段，通常用于 splice/tee/file page cache 快路径。
    pub(super) fn push_bufs(&mut self, bufs: Vec<PipeBuf>) {
        let len = bufs.iter().map(|buf| buf.len).sum::<usize>();
        assert!(len <= self.available_write(), "pipe buffer overflow");
        for buf in bufs {
            if buf.len > 0 {
                self.bufs.push_back(buf);
            }
        }
        self.bytes += len;
    }

    /// 从队头移出最多 len 字节的片段；必要时只切出队头片段的一部分。
    pub(super) fn pop_bufs(&mut self, len: usize) -> Vec<PipeBuf> {
        let mut remaining = len.min(self.available_read());
        let mut bufs = Vec::new();
        while remaining > 0 {
            let mut front = self.bufs.pop_front().unwrap();
            if front.len <= remaining {
                remaining -= front.len;
                self.bytes -= front.len;
                bufs.push(front);
            } else {
                let head = front.split_to(remaining);
                self.bytes -= remaining;
                remaining = 0;
                self.bufs.push_front(front);
                bufs.push(head);
            }
        }
        bufs
    }

    /// 克隆队头最多 len 字节的片段元数据，用于 tee 保留输入 pipe 数据。
    pub(super) fn clone_bufs(&self, len: usize) -> Vec<PipeBuf> {
        let mut remaining = len.min(self.available_read());
        let mut bufs = Vec::new();
        for buf in self.bufs.iter() {
            if remaining == 0 {
                break;
            }
            let copy_len = remaining.min(buf.len);
            bufs.push(PipeBuf {
                storage: buf.storage.clone(),
                offset: buf.offset,
                len: copy_len,
            });
            remaining -= copy_len;
        }
        bufs
    }

    /// 通过管道缓冲区读端弱指针判断管道的所有读端都被关闭
    pub(super) fn all_read_ends_closed(&self) -> bool {
        self.read_end_count == 0
    }

    /// 通过管道缓冲区写端弱指针判断管道的所有写端都被关闭
    pub(super) fn all_write_ends_closed(&self) -> bool {
        self.write_end_count == 0
    }

    pub(super) fn push_reader(&mut self, task: &Arc<TaskControlBlock>) {
        // 空管道读需要真正睡眠等待写者；只 yield 会让 lmbench lat_pipe 在内核里空转。
        if !self.read_waiters.iter().any(|waiter| {
            waiter
                .upgrade()
                .map_or(false, |waiter| Arc::ptr_eq(&waiter, task))
        }) {
            self.read_waiters.push_back(Arc::downgrade(task));
        }
    }

    pub(super) fn push_writer(&mut self, task: &Arc<TaskControlBlock>) {
        // 满管道写需要等待读者释放空间。
        if !self.write_waiters.iter().any(|waiter| {
            waiter
                .upgrade()
                .map_or(false, |waiter| Arc::ptr_eq(&waiter, task))
        }) {
            self.write_waiters.push_back(Arc::downgrade(task));
        }
    }

    fn wake_waiters(waiters: &mut VecDeque<Weak<TaskControlBlock>>, wake_one: bool) {
        while let Some(waiter) = waiters.pop_front() {
            if let Some(task) = waiter.upgrade() {
                let mut inner = task.inner_lock();
                if inner.task_status == TaskStatus::Blocked {
                    inner.task_status = TaskStatus::Ready;
                    drop(inner);
                    ready_queue::add_task(&task);
                    if wake_one {
                        break;
                    }
                }
            }
        }
    }

    pub(super) fn wake_reader(&mut self) {
        // pipe 每次写入只需要唤醒一个阻塞读者即可继续推进。
        Self::wake_waiters(&mut self.read_waiters, true);
        self.read_poll.wake();
    }

    pub(super) fn wake_writer(&mut self) {
        // pipe 每次读取释放空间后，只唤醒一个阻塞写者。
        Self::wake_waiters(&mut self.write_waiters, true);
        self.write_poll.wake();
    }

    pub(super) fn wake_all_readers(&mut self) {
        Self::wake_waiters(&mut self.read_waiters, false);
        self.read_poll.wake();
    }

    pub(super) fn wake_all_writers(&mut self) {
        Self::wake_waiters(&mut self.write_waiters, false);
        self.write_poll.wake();
    }
}
