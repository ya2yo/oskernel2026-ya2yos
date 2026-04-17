use alloc::sync::Arc;

use super::super::{File, Kstat};

mod simple_net;
pub use simple_net::*;
pub struct FakeSocket;

pub fn make_socket() -> Arc<dyn File> {
    Arc::new(FakeSocket {})
}

impl File for FakeSocket {
    fn readable(&self) -> bool {
        false
    }
    fn fstat(&self) -> Kstat {
        unimplemented!()
    }
}
