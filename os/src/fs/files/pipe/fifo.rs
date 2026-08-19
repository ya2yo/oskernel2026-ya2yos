//! FIFO 路径到共享管道缓冲区的生命周期映射。
//!
//! 全局表按 FIFO 路径保存弱引用，使同一路径打开的各端共享同一缓冲区；
//! 当最后一个端点释放后，弱引用自然失效。非阻塞方式单独打开写端且没有
//! 读端时遵循 Linux 语义返回 `ENXIO`。

use super::ring_buffer::PipeRingBuffer;
use super::Pipe;
use crate::fs::OpenFlags;
use crate::utils::SysErrNo;
use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::sync::{Arc, Weak};
use spin::{Lazy, Mutex};

static FIFO_BUFFERS: Lazy<Mutex<BTreeMap<String, Weak<Mutex<PipeRingBuffer>>>>> =
    Lazy::new(|| Mutex::new(BTreeMap::new()));

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
