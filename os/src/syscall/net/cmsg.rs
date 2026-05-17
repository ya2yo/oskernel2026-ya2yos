use alloc::vec;
use alloc::{slice, sync::Arc, vec::Vec};
use core::mem::{align_of, size_of};
use linux_raw_sys::net::{cmsghdr, SCM_RIGHTS, SOL_SOCKET};
// 假设这些是你项目中已有的定义
use crate::{
    fs::File,
    mm::UserBuffer,
    task::current_task,
    utils::{SysErrNo, SysResult},
};

const CMSG_ALIGN_SIZE: usize = size_of::<usize>();

/// 对应 C 语言的 CMSG_ALIGN
fn cmsg_align(len: usize) -> usize {
    (len + CMSG_ALIGN_SIZE - 1) & !(CMSG_ALIGN_SIZE - 1)
}

/// 对应 C 语言的 CMSG_LEN (头部 + 数据)
fn cmsg_len(body_len: usize) -> usize {
    size_of::<cmsghdr>() + body_len
}

/// 模拟 CMSG_SPACE 宏：头部长度 + 数据长度 + 对齐填充
fn cmsg_space(len: usize) -> usize {
    cmsg_align(size_of::<cmsghdr>() + len)
}

pub enum CMsg {
    Rights { fds: Vec<Arc<dyn File>> },
}

impl CMsg {
    pub fn parse(hdr: &cmsghdr, data_buffer: &[u8]) -> SysResult<Self> {
        let hdr_size = size_of::<cmsghdr>();
        if (hdr.cmsg_len as usize) < hdr_size {
            return Err(SysErrNo::EINVAL);
        }

        // data_buffer 应该是紧随 hdr 之后的数据部分
        let data_len = (hdr.cmsg_len as usize) - hdr_size;
        if data_len > data_buffer.len() {
            return Err(SysErrNo::EINVAL);
        }
        let data = &data_buffer[..data_len];

        match (hdr.cmsg_level as u32, hdr.cmsg_type as u32) {
            (SOL_SOCKET, SCM_RIGHTS) => {
                if data.len() % size_of::<i32>() != 0 {
                    return Err(SysErrNo::EINVAL);
                }
                let mut fds = Vec::new();
                for chunk in data.chunks_exact(size_of::<i32>()) {
                    let fd = i32::from_ne_bytes(chunk.try_into().unwrap());
                    if fd < 0 {
                        return Err(SysErrNo::EBADFD);
                    }

                    let fd_table = current_task().ok_or(SysErrNo::ESRCH)?.get_fd_table();
                    let f = fd_table.get(fd as usize)?.any();
                    fds.push(f);
                }
                Ok(Self::Rights { fds })
            }
            _ => Err(SysErrNo::EINVAL),
        }
    }
}

pub struct CMsgBuilder {
    buffer: UserBuffer,
    current_offset: usize,
}

impl CMsgBuilder {
    pub fn new(buffer: UserBuffer) -> Self {
        Self {
            buffer,
            current_offset: 0,
        }
    }

    /// 返回当前已填充的总长度（包含对齐填充）
    pub fn len(&self) -> usize {
        self.current_offset
    }

    pub fn push<F>(&mut self, level: u32, ty: u32, body_writer: F) -> SysResult<bool>
    where
        F: FnOnce(&mut [u8]) -> SysResult<usize>,
    {
        // 1. 确定起始位置并进行对齐
        let start_offset = cmsg_align(self.current_offset);
        let hdr_size = size_of::<cmsghdr>();

        // 2. 检查缓冲区剩余空间是否足够放下 Header
        if start_offset + hdr_size > self.buffer.len() {
            return Ok(false);
        }

        // 3. 在内核中先构造 Body 数据
        // 由于 UserBuffer 可能跨页，最稳妥的方法是先在内核申请一个临时 buffer
        // 或者是让 body_writer 直接往内核 buffer 写，然后再通过 UserBuffer 写入用户态
        let max_body_size = self.buffer.len() - (start_offset + hdr_size);
        let mut tmp_body = vec![0u8; core::cmp::min(max_body_size, 4096)]; // 限制大小防止溢出

        let actual_body_len = body_writer(&mut tmp_body)?;
        let total_msg_len = hdr_size + actual_body_len;

        // 4. 构造 Header
        let hdr = cmsghdr {
            cmsg_len: total_msg_len as _,
            cmsg_level: level as _,
            cmsg_type: ty as _,
        };

        // 5. 写入 Header 到 UserBuffer
        let hdr_bytes = unsafe { slice::from_raw_parts(&hdr as *const _ as *const u8, hdr_size) };
        if self.buffer.write_at(start_offset, hdr_bytes) != hdr_size as isize {
            return Err(SysErrNo::EFAULT);
        }

        // 6. 写入 Body 到 UserBuffer
        if self
            .buffer
            .write_at(start_offset + hdr_size, &tmp_body[..actual_body_len])
            != actual_body_len as isize
        {
            return Err(SysErrNo::EFAULT);
        }

        // 7. 更新 Offset 为对齐后的长度（为下一个 cmsg 做准备）
        self.current_offset = cmsg_align(start_offset + total_msg_len);

        Ok(true)
    }
}
