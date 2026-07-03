use log::{debug, warn};
use lwext4_rust::{
    Ext4File, InodeTypes,
    bindings::{O_CREAT, O_RDONLY, O_RDWR, O_TRUNC, SEEK_SET},
};

use crate::{
    fs::{
        FILE_PAGE_CACHE, Inode, InodeType, Kstat, OpenFlags, String, patch_dynamic_link_file_bytes,
    },
    sync::SyncUnsafeCell,
    utils::{SysErrNo, SysResult, SyscallRet},
};

use alloc::{format, string::ToString, vec};
use alloc::{sync::Arc, vec::Vec};

use lwext4_rust::file::OsDirent;

/// 防止符号链接死循环的最大跳转次数
const MAX_LOOPTIMES: usize = 5;

/// Ext4Inode 是对底层 Ext4File 的封装，实现了 VFS 的 Inode 接口
pub struct Ext4Inode {
    inner: SyncUnsafeCell<Ext4InodeInner>,
}

pub struct Ext4InodeInner {
    f: Ext4File,
    /// 指向同一 inode 的路径别名，用于 hard link / rename 后继续找到可用路径。
    aliases: Vec<String>,
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
                aliases: vec![path.to_string()],
                delay: false,
            }),
        }
    }

    fn add_alias_path(&self, path: &str) {
        let inner = self.inner.get_unchecked_mut();
        if inner.aliases.iter().all(|alias| alias != path) {
            inner.aliases.push(path.to_string());
        }
    }

    fn live_path(inner: &mut Ext4InodeInner) -> String {
        let current = inner.f.path().into_string().unwrap();
        let types = inner.f.types();
        if inner.f.check_inode_exist(&current, types.clone()) {
            return current;
        }

        for alias in inner.aliases.clone() {
            if inner.f.check_inode_exist(&alias, types.clone()) {
                let _ = inner.f.file_close();
                inner.f = Ext4File::new(&alias, types.clone());
                return alias;
            }
        }
        current
    }
}

impl Inode for Ext4Inode {
    /// 获取文件大小
    fn size(&self) -> usize {
        let inner = self.inner.get_unchecked_mut();
        let path = Self::live_path(inner);
        let types = as_inode_type(inner.f.file_type());
        if types == InodeType::File {
            let file = &mut inner.f;
            file.file_open(&path, O_RDONLY);
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
        let inner = self.inner.get_unchecked_mut();
        let _ = Self::live_path(inner);
        as_inode_type(inner.f.file_type())
    }

    /// 从指定偏移量读取数据到缓冲区
    fn read_at(&self, off: usize, buf: &mut [u8]) -> SyscallRet {
        let inner = self.inner.get_unchecked_mut();
        let path = Self::live_path(inner);
        let file = &mut inner.f;
        file.file_open(&path, O_RDONLY).map_err(SysErrNo::from)?;
        file.file_seek(off as i64, SEEK_SET)
            .map_err(SysErrNo::from)?;
        let r = file.file_read(buf).map_err(SysErrNo::from)?;
        patch_dynamic_link_file_bytes(&path, off, &mut buf[..r]);
        Ok(r)
    }

    fn write_at(&self, off: usize, buf: &[u8]) -> SyscallRet {
        let inner = self.inner.get_unchecked_mut();
        let path = Self::live_path(inner);
        let file = &mut inner.f;
        file.file_open(&path, O_RDWR).map_err(SysErrNo::from)?;
        file.file_seek(off as i64, SEEK_SET)
            .map_err(SysErrNo::from)?;
        let r = file.file_write(buf);
        r.map_err(SysErrNo::from)
    }

    /// 截断文件到指定长度
    fn truncate(&self, size: usize) -> SyscallRet {
        let inner = self.inner.get_unchecked_mut();
        let path = Self::live_path(inner);
        let file = &mut inner.f;
        file.file_open(&path, O_RDWR | O_CREAT | O_TRUNC)
            .map_err(SysErrNo::from)?;

        let t = file.file_truncate(size as u64);
        let ret = t.map_err(SysErrNo::from);
        if ret.is_ok() {
            FILE_PAGE_CACHE.invalidate_path(&path);
        }
        ret
    }

    fn rename(&self, path: &str, new_path: &str) -> SyscallRet {
        let file = &mut self.inner.get_unchecked_mut().f;
        let ret = file
            .file_rename(path, new_path)
            .map_or(Err(SysErrNo::ENOENT), |_| Ok(0));
        if ret.is_ok() {
            self.add_alias_path(new_path);
        }
        ret
    }

    /// 创建硬链接：hardlink_path 指向 old_path 相同的 inode
    fn hard_link(&self, old_path: &str, new_path: &str) -> SyscallRet {
        let file = &mut self.inner.get_unchecked_mut().f;
        let ret = file
            .file_hardlink(old_path, new_path)
            .map_or(Err(SysErrNo::ENOENT), |_| Ok(0));
        if ret.is_ok() {
            self.add_alias_path(new_path);
        }
        ret
    }

    fn set_timestamps(
        &self,
        atime: Option<u64>,
        mtime: Option<u64>,
        ctime: Option<u64>,
    ) -> SyscallRet {
        let inner = self.inner.get_unchecked_mut();
        let _ = Self::live_path(inner);
        let file = &mut inner.f;
        file.set_time(atime, mtime, ctime).map_err(SysErrNo::from)
    }

    /// 将文件缓存刷新到磁盘
    fn sync(&self) {
        let inner = self.inner.get_unchecked_mut();
        let _ = Self::live_path(inner);
        inner.f.file_cache_flush();
    }

    /// 一次性读取整个文件内容
    fn read_all(&self) -> Result<Vec<u8>, SysErrNo> {
        // 先提取 path 和类型，避免后续访问 self.inner 时产生重叠借用
        let (file_type, path_str) = {
            let inner = self.inner.get_unchecked_mut();
            let path = Self::live_path(inner);
            let file_type = as_inode_type(inner.f.types());
            (file_type, path)
        };

        if file_type == InodeType::File {
            let file = &mut self.inner.get_unchecked_mut().f;
            file.file_open(&path_str, O_RDONLY)
                .map_err(SysErrNo::from)?;
            let size = file.file_size() as usize;
            let mut buf: Vec<u8> = vec![0; size];
            file.file_seek(0, SEEK_SET).map_err(SysErrNo::from)?;
            let r = file.file_read(buf.as_mut_slice());
            if let Err(e) = r {
                Err(SysErrNo::from(e))
            } else {
                patch_dynamic_link_file_bytes(&path_str, 0, buf.as_mut_slice());
                Ok(buf)
            }
        } else if file_type == InodeType::SymLink {
            // 读取符号链接目标路径
            let mut real_path_buf = [0u8; 256];
            let link_file = Ext4Inode::new(&path_str, InodeTypes::EXT4_DE_SYMLINK);
            link_file.read_link(&mut real_path_buf, 256)?;
            let end = real_path_buf
                .iter()
                .position(|v| *v == 0)
                .unwrap_or(real_path_buf.len());
            let file_path =
                core::str::from_utf8(&real_path_buf[..end]).map_err(|_| SysErrNo::EINVAL)?;
            // 处理绝对/相对符号链接
            let next_path = if file_path.starts_with('/') {
                file_path.to_string()
            } else {
                join_path(&path_str, file_path)
            };
            // 通过 find 递归解析符号链接，然后读取目标文件内容
            let real_file = self.find(&next_path, OpenFlags::O_RDONLY, 0)?;
            real_file.read_all()
        } else {
            // 目录或其他不支持的类型
            Err(SysErrNo::EISDIR)
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
        if flags.contains(OpenFlags::O_NOFOLLOW) && file.is_symlink(path) {
            return Err(SysErrNo::ELOOP);
        }
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
            if flags.contains(OpenFlags::O_NOFOLLOW) {
                return Err(SysErrNo::ELOOP);
            }
            if loop_times >= MAX_LOOPTIMES {
                debug!("error ELOOP!");
                return Err(SysErrNo::ELOOP);
            }
            // 符号链接文件应该返回对应的真实的文件
            let mut file_name = [0u8; 256];
            let file = Ext4Inode::new(path, InodeTypes::EXT4_DE_SYMLINK);
            file.read_link(&mut file_name, 256)?;
            let end = file_name
                .iter()
                .position(|v| *v == 0)
                .unwrap_or(file_name.len());
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
        let inner = self.inner.get_unchecked_mut();
        let _ = Self::live_path(inner);
        let file = &mut inner.f;
        let stat = match file.fstat() {
            Ok(s) => s,
            Err(rc) => {
                warn!(
                    "Ext4Inode::fstat: ext4_stat_get failed rc={}, path={:?}",
                    rc,
                    file.path()
                );
                return Kstat::default();
            }
        };
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
    fn read_dentry(&self, off: usize, len: usize) -> SysResult<(Vec<u8>, isize)> {
        let inner = self.inner.get_unchecked_mut();
        let _ = Self::live_path(inner);
        let file = &mut inner.f;
        let entries = file.read_dir_from(off as u64).map_err(SysErrNo::from)?;
        let mut de: Vec<u8> = Vec::new();
        let (mut res, mut f_off) = (0usize, off);
        for entry in entries {
            if res + entry.len() > len {
                if res == 0 {
                    return Err(SysErrNo::EINVAL);
                }
                break;
            }
            res += entry.len();
            f_off = entry.off();
            de.extend_from_slice(entry.as_bytes());
        }
        // assert!(res != 0);
        Ok((de, f_off as isize))
    }

    fn is_dir_empty(&self) -> Result<bool, SysErrNo> {
        let inner = self.inner.get_unchecked_mut();
        let _ = Self::live_path(inner);
        let file = &mut inner.f;
        if as_inode_type(file.file_type()) != InodeType::Dir {
            return Err(SysErrNo::ENOTDIR);
        }

        let entries = file.read_dir_from(0).map_err(SysErrNo::from)?;
        for entry in entries {
            let name_end = entry
                .d_name
                .iter()
                .position(|ch| *ch == 0)
                .unwrap_or(entry.d_name.len());
            let name = &entry.d_name[..name_end];
            if name != b"." && name != b".." {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn read_link(&self, buf: &mut [u8], bufsize: usize) -> SysResult<usize> {
        let inner = self.inner.get_unchecked_mut();
        let _ = Self::live_path(inner);
        let file = &mut inner.f;
        file.file_readlink(buf, bufsize).map_err(SysErrNo::from)
    }

    fn sym_link(&self, target: &str, path: &str) -> SyscallRet {
        let file = &mut self.inner.get_unchecked_mut().f;
        file.file_fsymlink(target, path).map_err(SysErrNo::from)
    }
    /// 获取硬链接计数
    fn link_cnt(&self) -> SyscallRet {
        let inner = self.inner.get_unchecked_mut();
        let _ = Self::live_path(inner);
        let file = &mut inner.f;
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
        if self.types() == InodeType::Dir {
            file.dir_rm(path).map_err(SysErrNo::from)
        } else {
            file.file_remove(path).map_err(SysErrNo::from)
        }
    }

    fn path(&self) -> String {
        let inner = self.inner.get_unchecked_mut();
        Self::live_path(inner)
    }

    fn cache_path_alias(&self, path: &str) {
        self.add_alias_path(path);
    }
    fn delay(&self) {
        self.inner.get_unchecked_mut().delay = true;
    }

    fn fmode(&self) -> Result<u32, SysErrNo> {
        let inner = self.inner.get_unchecked_mut();
        let _ = Self::live_path(inner);
        let file = &mut inner.f;
        file.file_mode().map_err(SysErrNo::from)
    }
    fn fmode_set(&self, mode: u32) -> SyscallRet {
        let inner = self.inner.get_unchecked_mut();
        let _ = Self::live_path(inner);
        let file = &mut inner.f;
        let mode_type = mode & 0o170000;
        let mode_type = if mode_type != 0 {
            mode_type
        } else {
            as_inode_type(file.file_type()).mode_bits()
        };
        let mode = mode_type | (mode & 0o7777);
        file.file_mode_set(mode).map_err(SysErrNo::from)
    }

    fn owner_set(&self, uid: u32, gid: u32) -> SyscallRet {
        // Keep owner updates in the filesystem layer so stat and permission checks agree.
        let inner = self.inner.get_unchecked_mut();
        let _ = Self::live_path(inner);
        let file = &mut inner.f;
        file.file_owner_set(uid, gid).map_err(SysErrNo::from)
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
