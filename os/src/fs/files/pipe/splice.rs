use super::buffer::PipeBuf;
use super::Pipe;
use crate::fs::{File, FilePage};
use crate::utils::{SysErrNo, SyscallRet};
use alloc::sync::Arc;
use alloc::vec;

impl Pipe {
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
}
