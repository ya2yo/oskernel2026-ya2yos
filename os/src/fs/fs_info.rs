use alloc::string::{String, ToString};
use hashbrown::HashMap;
use log::debug;
pub struct FsInfo {
    /// 当前工作路径
    cwd: String,
    /// 可执行文件绝对路径
    exe: String,
    /// 一个文件对应多个fd
    fd2path: HashMap<usize, String>,
}

impl FsInfo {
    // ----------基本方法----------
    /// 只有initproc会调用
    pub fn new_for_initproc() -> Self {
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
    pub fn from_another(another: &FsInfo) -> Self {
        Self {
            cwd: another.cwd().to_string(),
            exe: another.exe().to_string(),
            fd2path: another.fd2path.clone(),
        }
    }
    pub fn clear(&mut self) {
        self.cwd.clear();
        self.exe.clear();
        self.fd2path.clear();
    }
    // ----------cwd----------
    pub fn cwd(&self) -> &str {
        self.cwd.as_str()
    }

    pub fn set_cwd(&mut self, cwd: String) {
        debug!("FsInfo::set_cwd: cwd is set to {}", cwd.as_str());
        self.cwd = cwd;
    }
    // ----------exe----------
    pub fn exe(&self) -> &str {
        &self.exe
    }

    pub fn set_exe(&mut self, exe: String) {
        self.exe = exe;
    }
    // ----------fd2path----------
    pub fn insert(&mut self, path: String, fd: usize) {
        self.fd2path.insert(fd, path);
    }
    pub fn insert_with_glue(&mut self, glue: usize, target: usize) {
        let path = self.fd2path.get(&glue).unwrap().clone();
        self.fd2path.insert(target, path);
    }
    pub fn has_fd(&self, path: &str) -> bool {
        self.fd2path.values().any(|v| v == path)
    }
    pub fn remove(&mut self, fd: usize) {
        self.fd2path.remove(&fd);
    }
}
