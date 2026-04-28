//! 文件系统的环境上下文
use alloc::string::{String, ToString};
use hashbrown::HashMap;
use log::debug;
use spin::{RwLock, RwLockReadGuard, RwLockWriteGuard};

/// 文件相关信息
pub struct FSInfo {
    inner: RwLock<FSInfoInner>,
}

struct FSInfoInner {
    /// 当前工作路径
    cwd: String,
    /// 可执行文件绝对路径
    exe: String,
    /// 一个文件对应多个fd
    fd2path: HashMap<usize, String>,
}

impl FSInfoInner {
    // ----------基本方法----------
    /// 只有initproc会调用
    fn new_for_initproc() -> Self {
        let mut fd2path = HashMap::new();
        fd2path.insert(0, "stdin".to_string());
        fd2path.insert(1, "stdout".to_string());
        fd2path.insert(2, "stderr".to_string());
        Self {
            cwd: String::from("/"),
            fd2path,
            exe: String::from("/initproc"),
        }
    }
    fn from_another(another: &FSInfoInner) -> Self {
        Self {
            cwd: another.get_cwd(),
            exe: another.get_exe(),
            fd2path: another.fd2path.clone(),
        }
    }
    fn clear(&mut self) {
        self.cwd.clear();
        self.exe.clear();
        self.fd2path.clear();
    }
    // ----------cwd----------
    fn get_cwd(&self) -> String {
        self.cwd.clone()
    }

    fn set_cwd(&mut self, cwd: String) {
        debug!("FSInfoInner::set_cwd: cwd is set to {}", cwd.as_str());
        self.cwd = cwd;
    }
    // ----------exe----------
    fn get_exe(&self) -> String {
        self.exe.clone()
    }

    fn set_exe(&mut self, exe: String) {
        self.exe = exe;
    }
    // ----------fd2path----------
    fn insert(&mut self, path: String, fd: usize) {
        self.fd2path.insert(fd, path);
    }
    fn has_fd(&self, path: &str) -> bool {
        self.fd2path.values().any(|v| v == path)
    }
    fn remove(&mut self, fd: usize) {
        self.fd2path.remove(&fd);
    }
}

impl FSInfo {
    /// 进程创建时调用
    pub fn new_initproc() -> Self {
        Self {
            inner: RwLock::new(FSInfoInner::new_for_initproc()),
        }
    }
    /// 通过已有对象进行创建
    pub fn from_another(another: &FSInfo) -> Self {
        let inner = another.inner.read();
        Self {
            inner: RwLock::new(FSInfoInner::from_another(&inner)),
        }
    }
    /// 清除自身
    pub fn clear(&self) {
        self.inner.write().clear();
    }
    /// 获取当前目录
    pub fn get_cwd(&self) -> String {
        self.inner.read().cwd.clone()
    }
    /// 修改当前目录
    pub fn set_cwd(&self, cwd: String) {
        debug!("FSInfoInner::set_cwd: cwd is set to {}", cwd.as_str());
        self.inner.write().cwd = cwd;
    }
    /// 获取可执行文件的绝对路径
    pub fn get_exe(&self) -> String {
        self.inner.read().exe.clone()
    }
    /// 设置可执行文件的绝对路径
    pub fn set_exe(&self, exe: String) {
        self.inner.write().exe = exe;
    }
    /// 文件描述符表相关操作
    /// 插入文件描述相关映射
    pub fn insert(&self, path: String, fd: usize) {
        self.inner.write().fd2path.insert(fd, path);
    }
    /// 用于 dup 等操作：将源 fd 的路径复制给目标 fd
    pub fn dup_fd_path(&self, old_fd: usize, new_fd: usize) {
        let mut inner = self.inner.write();
        // 如果旧 fd 有路径记录，则拷贝给新 fd
        if let Some(path) = inner.fd2path.get(&old_fd).cloned() {
            inner.fd2path.insert(new_fd, path);
        }
    }
    /// 检查当前是否打开了某个特定路径的文件
    pub fn has_fd(&self, path: &str) -> bool {
        self.inner.read().fd2path.values().any(|v| v == path)
    }
    /// 关闭文件时移除映射
    pub fn remove(&self, fd: usize) {
        self.inner.write().fd2path.remove(&fd);
    }
}

