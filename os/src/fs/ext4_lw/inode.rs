use log::{debug, warn};
use lwext4_rust::{
    bindings::{O_CREAT, O_RDONLY, O_RDWR, O_TRUNC, SEEK_SET},
    Ext4File, InodeTypes,
};

use crate::{
    fs::{Inode, InodeType, Kstat, OpenFlags, String},
    sync::SyncUnsafeCell,
    utils::{SysErrNo, SyscallRet},
};

use alloc::{format, string::ToString, vec};
use alloc::{sync::Arc, vec::Vec};

use super::dirent::Dirent;

/// 防止符号链接死循环的最大跳转次数
const MAX_LOOPTIMES: usize = 5;

/// Ext4Inode 是对底层 Ext4File 的封装，实现了 VFS 的 Inode 接口
pub struct Ext4Inode {
    inner: SyncUnsafeCell<Ext4InodeInner>,
}

pub struct Ext4InodeInner {
    f: Ext4File,
    /// 延迟删除标志。如果为 true，在该 Inode 被 Drop 时会从磁盘删除对应文件
    delay: bool,
}

unsafe impl Send for Ext4Inode {}
unsafe impl Sync for Ext4Inode {}

impl Ext4Inode {
    /// 创建一个新的 Ext4Inode 实例
    /// - `path`: 文件在 EXT4 内部的路径
    /// - `types`: 文件类型（文件、目录、链接等）
    pub fn new(path: &str, types: InodeTypes) -> Self {
        Ext4Inode {
            inner: SyncUnsafeCell::new(Ext4InodeInner {
                f: Ext4File::new(path, types),
                delay: false,
            }),
        }
    }
}

impl Inode for Ext4Inode {
    /// 获取文件大小
    fn size(&self) -> usize {
        let file = &mut self.inner.get_unchecked_mut().f;
        let types = as_inode_type(file.file_type());
        if types == InodeType::File {
            let path = file.path();
            let path = path.to_str().unwrap();
            file.file_open(path, O_RDONLY);
            let fsize = file.file_size();
            fsize as usize
        } else {
            0
        }
    }
    /// Ext4Inode创建必须使用绝对路径
    fn create(&self, path: &str, ty: InodeType) -> Result<Arc<dyn Inode>, SysErrNo> {
        let types = as_ext4_de_type(ty);
        let file = &mut self.inner.get_unchecked_mut().f;
        let nf = Ext4Inode::new(path, types.clone());

        if !file.check_inode_exist(path, types.clone()) {
            let nfile = &mut nf.inner.get_unchecked_mut().f;
            if types == InodeTypes::EXT4_DE_DIR {
                if let Err(e) = nfile.dir_mk(path) {
                    return Err(SysErrNo::from(e));
                }
            } else if let Err(e) = nfile.file_open(path, O_RDWR | O_CREAT | O_TRUNC) {
                return Err(SysErrNo::from(e));
            } else {
                nfile.file_close()?;
            }
        }
        Ok(Arc::new(nf))
    }

    fn types(&self) -> InodeType {
        as_inode_type(self.inner.get_unchecked_mut().f.file_type())
    }

    /// 从指定偏移量读取数据到缓冲区
    fn read_at(&self, off: usize, buf: &mut [u8]) -> SyscallRet {
        let file = &mut self.inner.get_unchecked_mut().f;
        let path = file.path();
        let path = path.to_str().unwrap();
        file.file_open(path, O_RDONLY).map_err(SysErrNo::from)?;
        file.file_seek(off as i64, SEEK_SET)
            .map_err(SysErrNo::from)?;
        let r = file.file_read(buf);
        r.map_err(SysErrNo::from)
    }

    fn write_at(&self, off: usize, buf: &[u8]) -> SyscallRet {
        let file = &mut self.inner.get_unchecked_mut().f;
        let path = file.path();
        let path = path.to_str().unwrap();
        file.file_open(path, O_RDWR).map_err(SysErrNo::from)?;
        file.file_seek(off as i64, SEEK_SET)
            .map_err(SysErrNo::from)?;
        let r = file.file_write(buf);
        r.map_err(SysErrNo::from)
    }

    /// 截断文件到指定长度
    fn truncate(&self, size: usize) -> SyscallRet {
        let file = &mut self.inner.get_unchecked_mut().f;
        let path = file.path();
        let path = path.to_str().unwrap();
        file.file_open(path, O_RDWR | O_CREAT | O_TRUNC)
            .map_err(SysErrNo::from)?;

        let t = file.file_truncate(size as u64);
        t.map_err(SysErrNo::from)
    }

    fn rename(&self, path: &str, new_path: &str) -> SyscallRet {
        let file = &mut self.inner.get_unchecked_mut().f;
        file.file_rename(path, new_path)
            .map_or(Err(SysErrNo::ENOENT), |_| Ok(0))
    }

    fn set_timestamps(
        &self,
        atime: Option<u64>,
        mtime: Option<u64>,
        ctime: Option<u64>,
    ) -> SyscallRet {
        let file = &mut self.inner.get_unchecked_mut().f;
        file.set_time(atime, mtime, ctime).map_err(SysErrNo::from)
    }

    /// 将文件缓存刷新到磁盘
    fn sync(&self) {
        self.inner.get_unchecked_mut().f.file_cache_flush();
    }

    /// 一次性读取整个文件内容
    fn read_all(&self) -> Result<Vec<u8>, SysErrNo> {
        let file = &mut self.inner.get_unchecked_mut().f;
        let file_type = as_inode_type(file.types());

        if file_type == InodeType::File {
            let path = file.path();
            let path = path.to_str().unwrap();
            file.file_open(path, O_RDONLY).map_err(SysErrNo::from)?;
            let size = file.file_size() as usize;
            let mut buf: Vec<u8> = vec![0; size];
            file.file_seek(0, SEEK_SET).map_err(SysErrNo::from)?;
            let r = file.file_read(buf.as_mut_slice());
            if let Err(e) = r {
                Err(SysErrNo::from(e))
            } else {
                Ok(buf)
            }
        } else {
            unimplemented!("not support!");
            // assert!(as_inode_type(file.types()) == InodeType::SymLink);
            // let mut real_path_buf = [0u8; 256];
            // file.file_readlink(&mut real_path_buf, 255)?;
            // let end = real_path_buf
            //     .iter()
            //     .enumerate()
            //     .find(|(_, v)| **v == 0)
            //     .map(|(idx, _)| idx)
            //     .unwrap();
            // let real_path = format!("/{}", core::str::from_utf8(&real_path_buf[..end]).unwrap());
            // debug!("[symlink] real_path= {}", real_path);
            // let real_file = self.find(&real_path)?;
            // real_file.read_all()
        }
    }

    /// 在路径中查找节点，支持递归解析符号链接
    fn find(
        &self,
        path: &str,
        flags: OpenFlags,
        loop_times: usize,
    ) -> Result<Arc<dyn Inode>, SysErrNo> {
        // log::info!("[Inode.find] origin path={}", path);
        let file = &mut self.inner.get_unchecked_mut().f;
        if file.check_inode_exist(path, InodeTypes::EXT4_DE_DIR) {
            Ok(Arc::new(Ext4Inode::new(path, InodeTypes::EXT4_DE_DIR)))
        } else if file.check_inode_exist(path, InodeTypes::EXT4_DE_REG_FILE) {
            if flags.contains(OpenFlags::O_DIRECTORY) {
                return Err(SysErrNo::ENOTDIR);
            }
            Ok(Arc::new(Ext4Inode::new(path, InodeTypes::EXT4_DE_REG_FILE)))
        } else if file.check_inode_exist(path, InodeTypes::EXT4_DE_SYMLINK) {
            if flags.contains(OpenFlags::O_UNLINK) {
                return Ok(Arc::new(Ext4Inode::new(path, InodeTypes::EXT4_DE_SYMLINK)));
            }
            if loop_times >= MAX_LOOPTIMES {
                debug!("error ELOOP!");
                return Err(SysErrNo::ELOOP);
            }
            // 符号链接文件应该返回对应的真实的文件
            let mut file_name = [0u8; 256];
            let file = Ext4Inode::new(path, InodeTypes::EXT4_DE_SYMLINK);
            file.read_link(&mut file_name, 256)?;
            let end = file_name.iter().position(|v| *v == 0).unwrap_or(file_name.len());
            let file_path = core::str::from_utf8(&file_name[..end]).unwrap();
            // log::info!("[Inode.find] file_path={}", file_path); 
            let next_path = if file_path.starts_with('/') {
                // 绝对路径 symlink
                file_path.to_string()
            } else {
                // 相对路径 symlink
                join_path(path, file_path)
            };
            //debug!("[Inode.find] symlink abs_path={}", &abs_path);
            self.find(&next_path, flags, loop_times + 1)
            // Ok(Arc::new(Ext4Inode::new(path, InodeTypes::EXT4_DE_SYMLINK)))
        } else {
            Err(SysErrNo::ENOENT)
        }
    }
    /// 获取文件状态信息
    fn fstat(&self) -> Kstat {
        let file = &mut self.inner.get_unchecked_mut().f;
        let stat = file.fstat().unwrap();
        let mut tmp_stat = stat; // ext4_inode_stat

        // 兼容性修补，处理时间戳高位。
        // lwext4 在某些实现中会将纳秒和秒混合存储在一个 u64 中，这里剥离出秒部分
        if tmp_stat.st_atime > (1 << 32) || tmp_stat.st_mtime > (1 << 32) {
            tmp_stat.st_ctime &= 0xFFFF_FFFF;
            tmp_stat.st_atime &= 0xFFFF_FFFF;
            tmp_stat.st_mtime &= 0xFFFF_FFFF;
        }
        Kstat {
            st_dev: stat.st_dev,
            st_ino: stat.st_ino,
            st_mode: stat.st_mode,
            st_nlink: stat.st_nlink,
            st_uid: stat.st_uid,
            st_gid: stat.st_gid,
            st_size: stat.st_size,
            st_blksize: stat.st_blksize,
            st_blocks: stat.st_blocks,
            st_atime: tmp_stat.st_atime,
            st_ctime: tmp_stat.st_ctime,
            st_mtime: tmp_stat.st_mtime,
            ..Kstat::default()
        }
    }
    /// 读取目录项内容
    fn read_dentry(&self, off: usize, len: usize) -> Result<(Vec<u8>, isize), SysErrNo> {
        let file = &mut self.inner.get_unchecked_mut().f;
        let entries = file.read_dir_from(off as u64).map_err(SysErrNo::from)?;
        let mut de: Vec<u8> = Vec::new();
        let (mut res, mut f_off) = (0usize, off);
        for entry in entries {
            let dirent = Dirent {
                d_ino: entry.d_ino,
                d_off: entry.d_off,
                d_reclen: entry.d_reclen,
                d_type: entry.d_type,
                d_name: entry.d_name,
            };
            if res + dirent.len() > len {
                break;
            }
            res += dirent.len();
            f_off = dirent.off();
            de.extend_from_slice(dirent.as_bytes());
        }
        // assert!(res != 0);
        Ok((de, f_off as isize))
    }

    fn read_link(&self, buf: &mut [u8], bufsize: usize) -> SyscallRet {
        let file = &mut self.inner.get_unchecked_mut().f;
        file.file_readlink(buf, bufsize).map_err(SysErrNo::from)
    }

    fn sym_link(&self, target: &str, path: &str) -> SyscallRet {
        let file = &mut self.inner.get_unchecked_mut().f;
        file.file_fsymlink(target, path).map_err(SysErrNo::from)
    }
    /// 获取硬链接计数
    fn link_cnt(&self) -> SyscallRet {
        let file = &mut self.inner.get_unchecked_mut().f;
        let r = file.links_cnt();
        if let Err(e) = r {
            if e == 2 {
                return Ok(0);
            } else {
                return Err(SysErrNo::from(e));
            }
        }
        Ok(r.unwrap() as usize)
    }

    fn unlink(&self, path: &str) -> SyscallRet {
        let file = &mut self.inner.get_unchecked_mut().f;
        file.file_remove(path).map_err(SysErrNo::from)
    }

    fn path(&self) -> String {
        self.inner
            .get_unchecked_ref()
            .f
            .path()
            .into_string()
            .unwrap()
    }
    fn delay(&self) {
        self.inner.get_unchecked_mut().delay = true;
    }

    fn fmode(&self) -> Result<u32, SysErrNo> {
        let file = &mut self.inner.get_unchecked_mut().f;
        file.file_mode().map_err(SysErrNo::from)
    }
    fn fmode_set(&self, mode: u32) -> SyscallRet {
        let file = &mut self.inner.get_unchecked_mut().f;
        file.file_mode_set(mode).map_err(SysErrNo::from)
    }
}

/// 当 Ext4Inode 声明周期结束时，确保关闭底层文件句柄
impl Drop for Ext4Inode {
    fn drop(&mut self) {
        let path = self.path();
        let inner = self.inner.get_unchecked_mut();
        // 如果标记了延时删除，则在关闭前移除文件
        if inner.delay {
            debug!("Ext4Inode delays unlink {:?}", path);
            inner.f.file_remove(&path);
        }
        inner.f.file_close().expect("failed to close fd");
    }
}

// --- 类型转换辅助函数 ---

/// 将内核 VFS 的 InodeType 转换为 EXT4 磁盘目录项类型
fn as_ext4_de_type(types: InodeType) -> InodeTypes {
    match types {
        InodeType::BlockDevice => InodeTypes::EXT4_DE_BLKDEV,
        InodeType::CharDevice => InodeTypes::EXT4_DE_CHRDEV,
        InodeType::Dir => InodeTypes::EXT4_DE_DIR,
        InodeType::Fifo => InodeTypes::EXT4_DE_FIFO,
        InodeType::File => InodeTypes::EXT4_DE_REG_FILE,
        InodeType::Socket => InodeTypes::EXT4_DE_SOCK,
        InodeType::SymLink => InodeTypes::EXT4_DE_SYMLINK,
        InodeType::Unknown => InodeTypes::EXT4_DE_UNKNOWN,
    }
}

/// 将底层磁盘读取到的类型转换为内核通用 InodeType
fn as_inode_type(types: InodeTypes) -> InodeType {
    match types {
        InodeTypes::EXT4_INODE_MODE_FIFO | InodeTypes::EXT4_DE_FIFO => InodeType::Fifo,
        InodeTypes::EXT4_INODE_MODE_CHARDEV | InodeTypes::EXT4_DE_CHRDEV => InodeType::CharDevice,
        InodeTypes::EXT4_INODE_MODE_DIRECTORY | InodeTypes::EXT4_DE_DIR => InodeType::Dir,
        InodeTypes::EXT4_INODE_MODE_BLOCKDEV | InodeTypes::EXT4_DE_BLKDEV => InodeType::BlockDevice,
        InodeTypes::EXT4_INODE_MODE_FILE | InodeTypes::EXT4_DE_REG_FILE => InodeType::File,
        InodeTypes::EXT4_INODE_MODE_SOFTLINK | InodeTypes::EXT4_DE_SYMLINK => InodeType::SymLink,
        InodeTypes::EXT4_INODE_MODE_SOCKET | InodeTypes::EXT4_DE_SOCK => InodeType::Socket,
        _ => {
            warn!("unknown file type: {:?}", types);
            unreachable!()
        }
    }
}
/// 路径规范函数
fn join_path(base: &str, rel: &str) -> String {
    let mut comps = Vec::new();

    for part in base.split('/') {
        if !part.is_empty() {
            comps.push(part);
        }
    }

    // 去掉当前文件名
    comps.pop();

    for part in rel.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                comps.pop();
            }
            x => comps.push(x),
        }
    }

    format!("/{}", comps.join("/"))
}
