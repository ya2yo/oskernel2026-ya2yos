//! 这是一个临时文件, 这里实现的是虚假的文件描述符供那些没有真正实现的文件描述符使用

pub struct DummyFd;
impl DummyFd {
    pub fn new()->Arc<Self>{
        DummyFd{}
    }
}
impl File for DummyFd {
    
}