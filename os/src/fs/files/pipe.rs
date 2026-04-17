// 该文件定义了一组特殊的文件：Pipe
// 它实现了File trait
// 它的特点是
use super::super::{File, StMode};
use crate::fs::Kstat;
use crate::task::{current_task, suspend_current_and_run_next};
use crate::utils::SysErrNo;
use crate::{mm::UserBuffer, syscall::PollEvents, utils::SyscallRet};
use alloc::sync::{Arc, Weak};
use alloc::vec;
use alloc::vec::Vec;
use core::cmp::min;
use spin::{Mutex, MutexGuard};

pub struct Pipe {
    readable: bool,
    writable: bool,
    buffer: Arc<Mutex<PipeRingBuffer>>,
}

impl Pipe {
    fn inner_lock(&self) -> MutexGuard<PipeRingBuffer> {
        self.buffer.lock()
    }
    /// 创建管道的读端
    fn read_end_with_buffer(buffer: Arc<Mutex<PipeRingBuffer>>) -> Self {
        Self {
            readable: true,
            writable: false,
            buffer,
        }
    }
    /// 创建管道的写端
    fn write_end_with_buffer(buffer: Arc<Mutex<PipeRingBuffer>>) -> Self {
        Self {
            readable: false,
            writable: true,
            buffer,
        }
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
}

/// 管道缓冲区状态
#[derive(Copy, Clone, PartialEq)]
enum RingBufferStatus {
    Full,
    Empty,
    Normal,
}

const RING_BUFFER_SIZE: usize = 65536;

/// Pipe的内层结构体
struct PipeRingBuffer {
    arr: Vec<u8>,
    head: usize,
    tail: usize,
    status: RingBufferStatus,
    write_end: Option<Weak<Pipe>>,
    read_end: Option<Weak<Pipe>>,
}

impl PipeRingBuffer {
    pub fn new() -> Self {
        Self {
            // arr: [0; RING_BUFFER_SIZE],
            arr: vec![0u8; RING_BUFFER_SIZE],
            head: 0,
            tail: 0,
            status: RingBufferStatus::Empty,
            write_end: None,
            read_end: None,
        }
    }
    pub fn set_write_end(&mut self, write_end: &Arc<Pipe>) {
        self.write_end = Some(Arc::downgrade(write_end));
    }
    pub fn set_read_end(&mut self, read_end: &Arc<Pipe>) {
        self.read_end = Some(Arc::downgrade(read_end));
    }
    /// 写一个字节到管道尾
    pub fn write_byte(&mut self, byte: u8) {
        self.status = RingBufferStatus::Normal;
        self.arr[self.tail] = byte;
        self.tail = (self.tail + 1) % RING_BUFFER_SIZE;
        if self.tail == self.head {
            self.status = RingBufferStatus::Full;
        }
    }
    /// 写n个字节到管道尾
    pub fn write_bytes(&mut self, bytes: &[u8], len: usize) {
        assert!(
            len <= RING_BUFFER_SIZE,
            "len must less than RING_BUFFER_SIZE"
        );
        self.status = RingBufferStatus::Normal;
        if self.tail + len <= RING_BUFFER_SIZE {
            self.arr[self.tail..self.tail + len].copy_from_slice(bytes);
        } else {
            let form_len = RING_BUFFER_SIZE - self.tail;
            let late_len = len - form_len;
            self.arr[self.tail..RING_BUFFER_SIZE].copy_from_slice(&bytes[..form_len]);
            self.arr[..late_len].copy_from_slice(&bytes[form_len..len]);
        }
        self.tail = (self.tail + len) % RING_BUFFER_SIZE;
        if self.tail == self.head {
            self.status = RingBufferStatus::Full;
        }
    }
    /// 从管道头读一个字节
    pub fn read_byte(&mut self) -> u8 {
        self.status = RingBufferStatus::Normal;
        let c = self.arr[self.head];
        self.head = (self.head + 1) % RING_BUFFER_SIZE;
        if self.head == self.tail {
            self.status = RingBufferStatus::Empty;
        }
        c
    }
    /// 从管道头读n个字节
    pub fn read_bytes(&mut self, len: usize) -> Vec<u8> {
        assert!(
            len <= RING_BUFFER_SIZE,
            "len must less than RING_BUFFER_SIZE"
        );
        self.status = RingBufferStatus::Normal;
        let mut bytes = vec![0; len];
        if self.head + len <= RING_BUFFER_SIZE {
            bytes[..].copy_from_slice(&self.arr[self.head..self.head + len]);
        } else {
            let form_len = RING_BUFFER_SIZE - self.head;
            let late_len = len - form_len;
            bytes[..form_len].copy_from_slice(&self.arr[self.head..RING_BUFFER_SIZE]);
            bytes[form_len..].copy_from_slice(&self.arr[..late_len]);
        }
        self.head = (self.head + len) % RING_BUFFER_SIZE;
        if self.head == self.tail {
            self.status = RingBufferStatus::Empty;
        }
        bytes
    }
    /// 获取管道中剩余可读长度
    pub fn available_read(&self) -> usize {
        if self.status == RingBufferStatus::Empty {
            0
        } else if self.tail > self.head {
            self.tail - self.head
        } else {
            self.tail + RING_BUFFER_SIZE - self.head
        }
    }
    /// 获取管道中剩余可写长度
    pub fn available_write(&self) -> usize {
        if self.status == RingBufferStatus::Full {
            0
        } else {
            RING_BUFFER_SIZE - self.available_read()
        }
    }
    /// 通过管道缓冲区读端弱指针判断管道的所有读端都被关闭
    pub fn all_read_ends_closed(&self) -> bool {
        self.read_end.as_ref().unwrap().upgrade().is_none()
    }
    /// 通过管道缓冲区写端弱指针判断管道的所有写端都被关闭
    pub fn all_write_ends_closed(&self) -> bool {
        self.write_end.as_ref().unwrap().upgrade().is_none()
    }
}

/// 创建一个管道并返回管道的读端和写端 (read_end, write_end)
pub fn make_pipe() -> (Arc<Pipe>, Arc<Pipe>) {
    let buffer = Arc::new(Mutex::new(PipeRingBuffer::new()));
    let read_end = Arc::new(Pipe::read_end_with_buffer(buffer.clone()));
    let write_end = Arc::new(Pipe::write_end_with_buffer(buffer.clone()));
    buffer.lock().set_read_end(&read_end);
    buffer.lock().set_write_end(&write_end);
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
                if ring_buffer.all_write_ends_closed() {
                    return Ok(read_size);
                }
                drop(ring_buffer);
                suspend_current_and_run_next();
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
        Ok(read_size)
    }
    fn write(&self, mut buf: UserBuffer) -> SyscallRet {
        assert!(self.writable());
        let mut write_size = 0usize;
        let mut loop_write;
        loop {
            let ring_buffer = self.inner_lock();
            loop_write = ring_buffer.available_write();
            if loop_write == 0 {
                drop(ring_buffer);
                suspend_current_and_run_next();
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
        Ok(write_size)
    }
    fn fstat(&self) -> Kstat {
        Kstat {
            st_mode: StMode::FIFO.bits(),
            st_nlink: 1,
            ..Kstat::default()
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
}
