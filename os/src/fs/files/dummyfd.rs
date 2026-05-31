//! 这是一个临时文件，这里实现的是虚假的文件描述符供那些没有真正实现的文件描述符使用

use alloc::sync::Arc;

use super::super::File;

pub struct DummyFd;

impl DummyFd {
    pub fn new() -> Arc<Self> {
        Arc::new(DummyFd {})
    }
}

impl File for DummyFd {
    fn readable(&self) -> bool {
        true
    }

    fn writable(&self) -> bool {
        true
    }
}