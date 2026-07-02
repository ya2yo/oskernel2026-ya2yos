use super::Pipe;
use super::ring_buffer::PipeRingBuffer;
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
