// 该文件实现匿名 pipe 和 FIFO 端点。
// Pipe 对外表现为 File trait，内部用按字节计数的 PipeBuf 片段队列保存数据。
// 普通 write 会拷贝用户数据生成 Bytes 片段；splice/tee 和 file page cache 路径可以通过
// 移动或克隆 PipeBuf 引用来减少跨 pipe 复制。
use super::super::{File, FilePage, OpenFlags, StMode};
use crate::fs::Kstat;
use crate::signal::{SigSet, check_if_any_sig_for_current_task, send_signal_to_thread};
use crate::task::{
    TaskControlBlock, TaskStatus, current_task, ready_queue, schedule_blocked_current,
};
use crate::utils::{PollSet, SysErrNo};
use crate::{
    mm::{UserBuffer, copy_to_user},
    syscall::PollEvents,
    utils::SyscallRet,
};
use alloc::collections::BTreeMap;
use alloc::collections::VecDeque;
use alloc::string::{String, ToString};
use alloc::sync::{Arc, Weak};
use alloc::vec;
use alloc::vec::Vec;
use core::cmp::min;
use core::sync::atomic::{AtomicBool, Ordering};
use core::task::Context;
use spin::{Lazy, Mutex, MutexGuard};

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
        flags: OpenFlags,
    ) -> Result<Self, SysErrNo> {
        let (readable, writable) = flags.read_write();
        if !readable && !writable {
            return Err(SysErrNo::EINVAL);
        }
        let pipe = Self::with_buffer(readable, writable, buffer);
        if flags.contains(OpenFlags::O_NONBLOCK) {
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
    pub fn all_read_ends_closed(&self) -> bool {
        self.inner_lock().all_read_ends_closed()
    }
    pub fn all_write_ends_closed(&self) -> bool {
        self.inner_lock().all_write_ends_closed()
    }
    /// Linux pipe 写入无读端时需要同时发送 SIGPIPE，并把 syscall 结果报告为 EPIPE。
    /// 调用该函数前不要持有 pipe/task 锁，避免 signal 路径和调度路径形成锁嵌套。
    fn broken_pipe() -> SysErrNo {
        if let Some(task) = current_task() {
            send_signal_to_thread(task.tid(), SigSet::SIGPIPE);
        }
        SysErrNo::EPIPE
    }
    /// pipe -> pipe 的 splice 快路径。
    /// 这里移动 PipeBuf 片段本身：Bytes 片段移动 Arc<Vec<u8>>，FilePage 片段移动 Arc<FilePage>，
    /// 因此两个 pipe 之间不需要重新复制片段里的实际数据。
    pub fn splice_to_pipe(&self, output: &Pipe, len: usize, nonblock: bool) -> SyscallRet {
        if !self.readable() || !output.writable() {
            return Err(SysErrNo::EBADF);
        }
        if Arc::ptr_eq(&self.buffer, &output.buffer) {
            return Err(SysErrNo::EINVAL);
        }
        self.wait_readable(nonblock)?;
        if self.available_read() == 0 {
            return Ok(0);
        }
        output.wait_writable(nonblock)?;

        let moved = self.with_ordered_buffers(output, |input, output| {
            if output.all_read_ends_closed() {
                return Err(SysErrNo::EPIPE);
            }
            let len = len
                .min(input.available_read())
                .min(output.available_write());
            if len == 0 {
                return Ok(0);
            }
            let bufs = input.pop_bufs(len);
            output.push_bufs(bufs);
            input.wake_writer();
            output.wake_reader();
            Ok(len)
        })?;
        Ok(moved)
    }
    /// tee 的语义是复制 pipe 数据到另一个 pipe，但不消费输入 pipe。
    /// clone_bufs 只克隆 PipeBuf 的引用和 offset/len 元数据，不复制底层字节。
    pub fn tee_to_pipe(&self, output: &Pipe, len: usize, nonblock: bool) -> SyscallRet {
        if !self.readable() || !output.writable() {
            return Err(SysErrNo::EBADF);
        }
        if Arc::ptr_eq(&self.buffer, &output.buffer) {
            return Err(SysErrNo::EINVAL);
        }
        self.wait_readable(nonblock)?;
        if self.available_read() == 0 {
            return Ok(0);
        }
        output.wait_writable(nonblock)?;

        let copied = self.with_ordered_buffers(output, |input, output| {
            if output.all_read_ends_closed() {
                return Err(SysErrNo::EPIPE);
            }
            let len = len
                .min(input.available_read())
                .min(output.available_write());
            if len == 0 {
                return Ok(0);
            }
            let bufs = input.clone_bufs(len);
            output.push_bufs(bufs);
            output.wake_reader();
            Ok(len)
        })?;
        Ok(copied)
    }
    /// splice(file, pipe) 使用的入口：把页缓存中的 FilePage 作为 PipeBuf 挂到 pipe 上。
    /// 读 pipe 时再从 FilePage 映射出的页内容拷贝到用户缓冲区，避免 file -> pipe 阶段复制数据。
    pub fn push_file_page(
        &self,
        page: Arc<FilePage>,
        page_offset: usize,
        len: usize,
        nonblock: bool,
    ) -> SyscallRet {
        if !self.writable() {
            return Err(SysErrNo::EBADF);
        }
        if len == 0 {
            return Ok(0);
        }
        self.wait_writable(nonblock)?;
        let mut ring_buffer = self.inner_lock();
        if ring_buffer.all_read_ends_closed() {
            return Err(SysErrNo::EPIPE);
        }
        let len = len.min(ring_buffer.available_write());
        if len == 0 {
            return Ok(0);
        }
        ring_buffer.push_bufs(vec![PipeBuf::from_file_page(page, page_offset, len)]);
        ring_buffer.wake_reader();
        Ok(len)
    }
    /// 等待 pipe 变为可读。阻塞前先把当前任务置为 Blocked，再重新检查条件并入队，
    /// 这样可以覆盖“解锁后、入队前”写者唤醒造成的竞态窗口。
    fn wait_readable(&self, nonblock: bool) -> Result<(), SysErrNo> {
        loop {
            let ring_buffer = self.inner_lock();
            if ring_buffer.available_read() > 0 || ring_buffer.all_write_ends_closed() {
                return Ok(());
            }
            if nonblock || self.nonblocking() {
                return Err(SysErrNo::EAGAIN);
            }
            drop(ring_buffer);
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
            schedule_blocked_current(task_cx_ptr);
        }
    }
    /// 等待 pipe 变为可写。逻辑与 wait_readable 对称，同时需要处理所有读端关闭时的 EPIPE。
    fn wait_writable(&self, nonblock: bool) -> Result<(), SysErrNo> {
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
            schedule_blocked_current(task_cx_ptr);
        }
    }
    /// 同时锁两个 pipe buffer 时按 Arc 地址排序，保证所有调用点使用一致锁顺序。
    /// splice/tee 需要同时观察输入和输出 pipe，固定锁顺序可以避免两个方向并发操作时死锁。
    fn with_ordered_buffers<T>(
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

pub fn open_fifo(path: &str, flags: OpenFlags) -> Result<Arc<Pipe>, SysErrNo> {
    let mut table = FIFO_BUFFERS.lock();
    let buffer = match table.get(path).and_then(|buffer| buffer.upgrade()) {
        Some(buffer) => buffer,
        None => {
            let buffer = Arc::new(Mutex::new(PipeRingBuffer::new()));
            table.insert(path.to_string(), Arc::downgrade(&buffer));
            buffer
        }
    };

    let (readable, writable) = flags.read_write();
    if writable
        && !readable
        && flags.contains(OpenFlags::O_NONBLOCK)
        && buffer.lock().all_read_ends_closed()
    {
        return Err(SysErrNo::ENXIO);
    }

    Ok(Arc::new(Pipe::fifo_end_with_buffer(buffer, flags)?))
}

const RING_BUFFER_SIZE: usize = 65536;
const FIONREAD: u32 = 0x541B;
const IOC_WATCH_QUEUE_SET_SIZE: u32 = 0x5760;
const IOC_WATCH_QUEUE_SET_FILTER: u32 = 0x5761;

/// pipe 中的一个连续数据片段。
/// offset/len 描述当前片段在底层存储中的窗口，split_to 可以只切出头部而不复制数据。
#[derive(Clone)]
struct PipeBuf {
    storage: PipeBufStorage,
    offset: usize,
    len: usize,
}

/// PipeBuf 的底层存储来源。
/// Bytes 来自普通 write，FilePage 来自 page cache；二者都通过 Arc 支持 splice/tee 的引用移动/复制。
#[derive(Clone)]
enum PipeBufStorage {
    Bytes(Arc<Vec<u8>>),
    FilePage(Arc<FilePage>),
}

impl PipeBuf {
    fn new(bytes: Vec<u8>) -> Self {
        let len = bytes.len();
        Self {
            storage: PipeBufStorage::Bytes(Arc::new(bytes)),
            offset: 0,
            len,
        }
    }
    fn from_file_page(page: Arc<FilePage>, offset: usize, len: usize) -> Self {
        Self {
            storage: PipeBufStorage::FilePage(page),
            offset,
            len,
        }
    }
    fn split_to(&mut self, len: usize) -> Self {
        let len = len.min(self.len);
        let buf = Self {
            storage: self.storage.clone(),
            offset: self.offset,
            len,
        };
        self.offset += len;
        self.len -= len;
        buf
    }
    /// 读 pipe 时统一把片段转换成字节切片；FilePage 分支直接访问页帧内容。
    fn as_slice(&self) -> &[u8] {
        match &self.storage {
            PipeBufStorage::Bytes(data) => &data[self.offset..self.offset + self.len],
            PipeBufStorage::FilePage(page) => {
                let bytes = page.frame.ppn.bytes_array();
                &bytes[self.offset..self.offset + self.len]
            }
        }
    }
}

/// Pipe 的内层共享缓冲区。
/// 名称保留 RingBuffer，但实际已经是按字节容量限制的 PipeBuf 片段队列。
struct PipeRingBuffer {
    bufs: VecDeque<PipeBuf>,
    bytes: usize,
    write_end_count: usize,
    read_end_count: usize,
    read_waiters: VecDeque<Weak<TaskControlBlock>>,
    write_waiters: VecDeque<Weak<TaskControlBlock>>,
    read_poll: PollSet,
    write_poll: PollSet,
}

static FIFO_BUFFERS: Lazy<Mutex<BTreeMap<String, Weak<Mutex<PipeRingBuffer>>>>> =
    Lazy::new(|| Mutex::new(BTreeMap::new()));

impl PipeRingBuffer {
    pub fn new() -> Self {
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
    pub fn write_byte(&mut self, byte: u8) {
        self.write_bytes(&[byte], 1);
    }
    /// 写n个字节到管道尾
    pub fn write_bytes(&mut self, bytes: &[u8], len: usize) {
        assert!(len <= self.available_write(), "pipe buffer overflow");
        let len = len.min(bytes.len());
        if len == 0 {
            return;
        }
        self.bufs.push_back(PipeBuf::new(bytes[..len].to_vec()));
        self.bytes += len;
    }
    /// 从管道头读一个字节
    pub fn read_byte(&mut self) -> u8 {
        self.read_bytes(1)[0]
    }
    /// 从管道头读n个字节
    pub fn read_bytes(&mut self, len: usize) -> Vec<u8> {
        let len = len.min(self.available_read());
        let mut bytes = Vec::with_capacity(len);
        for buf in self.pop_bufs(len) {
            bytes.extend_from_slice(buf.as_slice());
        }
        bytes
    }
    /// 获取管道中剩余可读长度
    pub fn available_read(&self) -> usize {
        self.bytes
    }
    /// 获取管道中剩余可写长度
    pub fn available_write(&self) -> usize {
        RING_BUFFER_SIZE - self.bytes
    }
    /// 追加一组片段，通常用于 splice/tee/file page cache 快路径。
    fn push_bufs(&mut self, bufs: Vec<PipeBuf>) {
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
    fn pop_bufs(&mut self, len: usize) -> Vec<PipeBuf> {
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
    fn clone_bufs(&self, len: usize) -> Vec<PipeBuf> {
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
    pub fn all_read_ends_closed(&self) -> bool {
        self.read_end_count == 0
    }
    /// 通过管道缓冲区写端弱指针判断管道的所有写端都被关闭
    pub fn all_write_ends_closed(&self) -> bool {
        self.write_end_count == 0
    }
    fn push_reader(&mut self, task: &Arc<TaskControlBlock>) {
        // 空管道读需要真正睡眠等待写者；只 yield 会让 lmbench lat_pipe 在内核里空转。
        if !self.read_waiters.iter().any(|waiter| {
            waiter
                .upgrade()
                .map_or(false, |waiter| Arc::ptr_eq(&waiter, task))
        }) {
            self.read_waiters.push_back(Arc::downgrade(task));
        }
    }
    fn push_writer(&mut self, task: &Arc<TaskControlBlock>) {
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

    fn wake_reader(&mut self) {
        // pipe 每次写入只需要唤醒一个阻塞读者即可继续推进。
        Self::wake_waiters(&mut self.read_waiters, true);
        self.read_poll.wake();
    }

    fn wake_writer(&mut self) {
        // pipe 每次读取释放空间后，只唤醒一个阻塞写者。
        Self::wake_waiters(&mut self.write_waiters, true);
        self.write_poll.wake();
    }

    fn wake_all_readers(&mut self) {
        Self::wake_waiters(&mut self.read_waiters, false);
        self.read_poll.wake();
    }

    fn wake_all_writers(&mut self) {
        Self::wake_waiters(&mut self.write_waiters, false);
        self.write_poll.wake();
    }
}

/// 创建一个管道并返回管道的读端和写端 (read_end, write_end)
pub fn make_pipe() -> (Arc<Pipe>, Arc<Pipe>) {
    let buffer = Arc::new(Mutex::new(PipeRingBuffer::new()));
    let read_end = Arc::new(Pipe::read_end_with_buffer(buffer.clone()));
    let write_end = Arc::new(Pipe::write_end_with_buffer(buffer.clone()));
    (read_end, write_end)
}

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
