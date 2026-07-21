use core::ffi::c_char;

use crate::bindings::*;
use alloc::collections::{BTreeMap, VecDeque};
use alloc::string::String;
use alloc::sync::Arc;
use alloc::{ffi::CString, vec::Vec};
use spin::{Lazy, Mutex, RwLock};

const PAGE_SIZE: usize = 4096;
pub const PAGE_MASK: usize = !0xfff;
// Covers iozone -s 4m while still avoiding large test artifacts.
const MAX_CACHED_FILE_SIZE: usize = 4 * 0x10_0000; // 4 MiB

fn aligned_down(addr: usize) -> usize {
    addr & PAGE_MASK
}

/// Convert ext4 on-disk directory entry types to the Linux `getdents64`
/// `d_type` ABI.  The two enums use different numeric values.
#[inline]
fn ext4_dirent_type_to_linux_dtype(inode_type: u8) -> u8 {
    const DT_UNKNOWN: u8 = 0;
    const DT_FIFO: u8 = 1;
    const DT_CHR: u8 = 2;
    const DT_DIR: u8 = 4;
    const DT_BLK: u8 = 6;
    const DT_REG: u8 = 8;
    const DT_LNK: u8 = 10;
    const DT_SOCK: u8 = 12;

    match inode_type as u32 {
        EXT4_DE_UNKNOWN => DT_UNKNOWN,
        EXT4_DE_REG_FILE => DT_REG,
        EXT4_DE_DIR => DT_DIR,
        EXT4_DE_CHRDEV => DT_CHR,
        EXT4_DE_BLKDEV => DT_BLK,
        EXT4_DE_FIFO => DT_FIFO,
        EXT4_DE_SOCK => DT_SOCK,
        EXT4_DE_SYMLINK => DT_LNK,
        _ => DT_UNKNOWN,
    }
}

// Ext4File文件操作与block device设备解耦了
pub struct Ext4File {
    //file_desc_map: BTreeMap<CString, ext4_file>,
    file_desc: ext4_file,
    file_path: CString,

    this_type: InodeTypes,

    has_opened: bool,
    last_flags: u32,
    pending_mode: Option<u32>,
    // Large files bypass the whole-file write-back cache after one size probe.
    cache_too_large: bool,
}

impl Ext4File {
    pub fn new(path: &str, types: InodeTypes) -> Self {
        Self {
            file_desc: ext4_file {
                mp: core::ptr::null_mut(),
                inode: 0,
                flags: 0,
                fsize: 0,
                fpos: 0,
            },
            file_path: CString::new(path).expect("CString::new Ext4File path failed"),
            this_type: types,
            has_opened: false,
            last_flags: 0,
            pending_mode: None,
            cache_too_large: false,
        }
    }

    pub fn path(&self) -> CString {
        self.file_path.clone()
    }

    pub fn types(&self) -> InodeTypes {
        self.this_type.clone()
    }

    /// File open function.
    ///
    /// |---------------------------------------------------------------|
    /// |   r or rb                 O_RDONLY                            |
    /// |---------------------------------------------------------------|
    /// |   w or wb                 O_WRONLY|O_CREAT|O_TRUNC            |
    /// |---------------------------------------------------------------|
    /// |   a or ab                 O_WRONLY|O_CREAT|O_APPEND           |
    /// |---------------------------------------------------------------|
    /// |   r+ or rb+ or r+b        O_RDWR                              |
    /// |---------------------------------------------------------------|
    /// |   w+ or wb+ or w+b        O_RDWR|O_CREAT|O_TRUNC              |
    /// |---------------------------------------------------------------|
    /// |   a+ or ab+ or a+b        O_RDWR|O_CREAT|O_APPEND             |
    /// |---------------------------------------------------------------|
    pub fn file_open(&mut self, path: &str, flags: u32) -> Result<usize, i32> {
        let c_path = CString::new(path).expect("CString::new failed");
        if c_path != self.path() {
            // debug!(
            //     "Ext4File file_open, cur path={}, new path={}",
            //     self.file_path.to_str().unwrap(),
            //     path
            // );
        } else {
            if self.has_opened && self.last_flags == flags {
                //如果之前已经按相同方式打开
                //debug!("reopen");
                return Ok(EOK as usize);
            }
        }

        //let to_map = c_path.clone();
        let c_path = c_path.into_raw();
        let c_flags = Self::flags_to_cstring(flags);
        let c_flags = c_flags.into_raw();

        let r = unsafe { ext4_fopen(&mut self.file_desc, c_path, c_flags) };
        unsafe {
            // deallocate the CString
            drop(CString::from_raw(c_path));
            drop(CString::from_raw(c_flags));
        }
        if r != EOK as i32 {
            error!("ext4_fopen: {}, rc = {}", path, r);
            return Err(r);
        }

        self.has_opened = true;
        self.last_flags = flags;
        if flags & O_TRUNC != 0 {
            self.cache_too_large = false;
        }

        //self.file_desc_map.insert(to_map, fd); // store c_path
        //debug!("file_open {}, mp={:#x}", path, self.file_desc.mp as usize);
        Ok(EOK as usize)
    }

    pub fn file_close(&mut self) -> Result<usize, i32> {
        if self.file_desc.mp != core::ptr::null_mut() {
            //debug!("file_close {:?}", self.get_path());
            self.file_cache_flush()?;
            unsafe {
                ext4_fclose(&mut self.file_desc);
            }
        }

        self.has_opened = false;

        Ok(0)
    }

    pub fn flags_to_cstring(flags: u32) -> CString {
        let cstr = match flags {
            O_RDONLY => "rb",
            O_RDWR => "r+",
            0x241 => "wb", // O_WRONLY | O_CREAT | O_TRUNC
            0x441 => "ab", // O_WRONLY | O_CREAT | O_APPEND
            0x242 => "w+", // O_RDWR | O_CREAT | O_TRUNC
            0x442 => "a+", // O_RDWR | O_CREAT | O_APPEND
            _ => {
                warn!("Unknown File Open Flags: {:#x}", flags);
                "r+"
            }
        };
        //debug!("flags_to_cstring: {}", cstr);
        CString::new(cstr).expect("CString::new OpenFlags failed")
    }

    /// Inode types:
    /// EXT4_DIRENTRY_UNKNOWN
    /// EXT4_DE_REG_FILE
    /// EXT4_DE_DIR
    /// EXT4_DE_CHRDEV
    /// EXT4_DE_BLKDEV
    /// EXT4_DE_FIFO
    /// EXT4_DE_SOCK
    /// EXT4_DE_SYMLINK
    ///
    /// Check if inode exists.
    pub fn check_inode_exist(&mut self, path: &str, types: InodeTypes) -> bool {
        let c_path = CString::new(path).expect("CString::new failed");
        let c_path = c_path.into_raw();
        // let mtype = types.clone();
        let r = unsafe { ext4_inode_exist(c_path, types as i32) }; //eg: types: EXT4_DE_REG_FILE
        unsafe {
            drop(CString::from_raw(c_path));
        }
        if r == EOK as i32 {
            // debug!("{:?} {} Exist", mtype, path);
            true //Exist
        } else {
            // debug!("{:?} {} No Exist. ext4_inode_exist rc = {}", mtype, path, r);
            false
        }
    }

    pub fn file_readlink(&mut self, buf: &mut [u8], bufsize: usize) -> Result<usize, i32> {
        let c_path = self.file_path.clone();
        let c_path = c_path.into_raw();
        let buf_c_char = buf.as_mut_ptr() as *mut c_char;
        let mut rcnt = 0usize;
        let r = unsafe { ext4_readlink(c_path, buf_c_char, bufsize, &mut rcnt) };
        unsafe {
            drop(CString::from_raw(c_path));
        }
        if r != EOK as i32 {
            error!("ext4_readlink error: rc = {}", r);
            return Err(r);
        }
        Ok(rcnt)
    }

    pub fn is_symlink(&mut self, path: &str) -> bool {
        let c_path = CString::new(path).expect("CString::new failed");
        let c_path = c_path.into_raw();
        let mut buf = [0 as c_char; 1];
        let mut rcnt = 0usize;
        let r = unsafe { ext4_readlink(c_path, buf.as_mut_ptr(), buf.len(), &mut rcnt) };
        unsafe {
            drop(CString::from_raw(c_path));
        }
        r == EOK as i32
    }

    //ext4_fsymlink(const char *target, const char *path)
    pub fn file_fsymlink(&mut self, target: &str, path: &str) -> Result<usize, i32> {
        let c_path = CString::new(path).expect("CString::new failed");
        let c_path = c_path.into_raw();
        let target_path = CString::new(target).expect("CString::new failed");
        let target_path = target_path.into_raw();
        let r = unsafe { ext4_fsymlink(target_path, c_path) };
        if r != EOK as i32 {
            error!("ext4_fsymlink error: rc = {}", r);
            return Err(r);
        }
        Ok(EOK as usize)
    }

    /// Rename file and directory
    pub fn file_rename(&mut self, path: &str, new_path: &str) -> Result<usize, i32> {
        let c_path = CString::new(path).expect("CString::new failed");
        let c_path = c_path.into_raw();
        let c_new_path = CString::new(new_path).expect("CString::new failed");
        let c_new_path = c_new_path.into_raw();
        let r = unsafe { ext4_frename(c_path, c_new_path) };
        unsafe {
            drop(CString::from_raw(c_path));
            drop(CString::from_raw(c_new_path));
        }
        if r != EOK as i32 {
            error!("ext4_frename error: rc = {}", r);
            return Err(r);
        }
        Ok(EOK as usize)
    }

    /// Create a hard link for a file.
    /// - `path`: Path to existing file.
    /// - `hardlink_path`: Path of the new hard link (must not exist).
    pub fn file_hardlink(&mut self, path: &str, hardlink_path: &str) -> Result<usize, i32> {
        let c_path = CString::new(path).expect("CString::new failed");
        let c_path = c_path.into_raw();
        let c_hardlink_path = CString::new(hardlink_path).expect("CString::new failed");
        let c_hardlink_path = c_hardlink_path.into_raw();
        let r = unsafe { ext4_flink(c_path, c_hardlink_path) };
        unsafe {
            drop(CString::from_raw(c_path));
            drop(CString::from_raw(c_hardlink_path));
        }
        if r != EOK as i32 {
            error!("ext4_flink error: rc = {}", r);
            return Err(r);
        }
        Ok(EOK as usize)
    }

    /// Remove file by path.
    pub fn file_remove(&mut self, path: &str) -> Result<usize, i32> {
        //debug!("file_remove {}", path);
        let c_path = CString::new(path).expect("CString::new failed");
        let c_path = c_path.into_raw();

        let path = String::from(path);
        // 删除目录项后，该路径不能继续保留 write-back cache 或 FIFO 元数据。
        remove_file_cache_state(&path);

        //修改为未打开
        self.has_opened = false;

        let r = unsafe { ext4_fremove(c_path) };
        unsafe {
            drop(CString::from_raw(c_path));
        }
        if (r != EOK as i32) && (r != ENOENT as i32) {
            error!("ext4_fremove error: rc = {}", r);
            return Err(r);
        }
        self.cache_too_large = false;
        Ok(EOK as usize)
    }

    // 检查是否值得建立文件缓存。大文件直接走 ext4，避免一次性占用大量堆。
    fn check_cached(&mut self, file_path: String) {
        // These files are regenerated and synchronously synced on every procfs
        // refresh. Keeping them in the global delayed write-back FIFO leaves
        // stale entries after process teardown and can recurse into lwext4
        // while an ext4 operation is already in progress.
        if is_proc_task_runtime_file(&file_path) {
            return;
        }

        if if_cache(file_path.clone()) {
            return;
        }

        if self.cache_too_large {
            return;
        }

        let c_path = CString::new(file_path.as_str()).expect("CString::new failed");
        let c_path = c_path.into_raw();
        let c_flags = Ext4File::flags_to_cstring(2).into_raw();
        let mut cache_desc = ext4_file {
            mp: core::ptr::null_mut(),
            inode: 0,
            flags: 0,
            fsize: 0,
            fpos: 0,
        };

        // Query cache contents through a separate descriptor.  Reopening
        // self.file_desc here can otherwise desynchronize an active file from
        // its path/cache key when lwext4 rejects the cache open.
        let r = unsafe { ext4_fopen(&mut cache_desc, c_path, c_flags) };
        unsafe {
            drop(CString::from_raw(c_path));
            drop(CString::from_raw(c_flags));
        }
        if r != EOK as i32 {
            error!("check_cached ext4_fopen: {}, rc = {}", file_path, r);
            return;
        }

        let size = unsafe { ext4_fsize(&mut cache_desc) as usize };
        if size > MAX_CACHED_FILE_SIZE {
            unsafe {
                ext4_fclose(&mut cache_desc);
            }
            self.cache_too_large = true;
            return;
        }

        let cache = Arc::new(RwLock::new(VFileCache::new()));
        let mut cache_writer = cache.write();
        cache_writer.mode = self.pending_mode;
        let aligned_size = aligned_down(size) + PAGE_SIZE;
        cache_writer.data = Vec::new();
        if cache_writer.data.try_reserve_exact(aligned_size).is_err() {
            unsafe {
                ext4_fclose(&mut cache_desc);
            }
            return;
        }
        let data = &mut cache_writer.data;
        unsafe {
            data.set_len(aligned_size);
        }
        cache_writer.size = size;
        if size == 0 {
            unsafe {
                ext4_fclose(&mut cache_desc);
            }
            insert_cache(file_path.clone(), &cache);
            insert_fifo(file_path.clone());
            debug!("initialize cache! {}", file_path);
            return;
        }
        unsafe { ext4_fseek(&mut cache_desc, 0, SEEK_SET) };
        let mut rw_count = 0;
        unsafe {
            ext4_fread(
                &mut cache_desc,
                cache_writer.data.as_mut_ptr() as _,
                size,
                &mut rw_count,
            )
        };
        unsafe {
            ext4_fclose(&mut cache_desc);
        }
        insert_cache(file_path.clone(), &cache);
        insert_fifo(file_path.clone());
        debug!("initialize cache! {}", file_path);
    }

    pub fn file_seek(&mut self, offset: i64, seek_type: u32) -> Result<usize, i32> {
        if self.this_type != InodeTypes::EXT4_DE_DIR {
            let path = String::from((*self.file_path).to_str().unwrap());
            if !self.cache_too_large {
                self.check_cached(path.clone());
            }

            if if_cache(path.clone()) {
                let cache = get_cache(path.clone());
                let mut cache_writer = cache.write();
                cache_writer.offset =
                    seek_pos(cache_writer.offset, cache_writer.size, offset, seek_type)?;
                return Ok(EOK as usize);
            }
        }

        self.file_desc.fpos = seek_pos(
            self.file_desc.fpos as usize,
            self.file_desc.fsize as usize,
            offset,
            seek_type,
        )? as u64;

        Ok(EOK as usize)
    }

    pub fn file_read(&mut self, buff: &mut [u8]) -> Result<usize, i32> {
        let path = String::from((*self.file_path).to_str().unwrap());
        if if_cache(path.clone()) {
            //找到cache直接读cache
            let cache = get_cache(path.clone());
            let cache_read = cache.read();
            let data = cache_read.get_data_slice();
            if cache_read.offset >= cache_read.size {
                return Ok(0);
            }
            let length = buff.len();
            let end = cache_read
                .offset
                .checked_add(length)
                .ok_or(EINVAL as i32)?
                .min(cache_read.size);
            let r_sz = end - cache_read.offset;
            //debug!("data.len={:x},end={:x}", data.len(), end);
            if length <= 10 {
                for i in 0..r_sz {
                    buff[i] = data[cache_read.offset + i];
                }
            } else {
                buff[..r_sz].copy_from_slice(&data[cache_read.offset..end]);
            }

            /*
            debug!(
                "file_read {},len = {:x},offset is {:x}",
                path, r_sz, cache_read.offset
            );
            */

            return Ok(r_sz);
        }

        let mut rw_count = 0;
        let r = unsafe {
            ext4_fread(
                &mut self.file_desc,
                buff.as_mut_ptr() as _,
                buff.len(),
                &mut rw_count,
            )
        };

        if r != EOK as i32 {
            error!("ext4_fread: rc = {}", r);
            return Err(r);
        }

        //debug!("file_read {:?}, len={}", self.get_path(), rw_count);
        Ok(rw_count)
    }

    /*
    pub fn file_close(&mut self, path: &str) -> Result<usize, i32> {
        let cstr_path = CString::new(path).unwrap();
        if let Some(mut fd) = self.file_desc_map.remove(&cstr_path) {
            unsafe {
                ext4_fclose(&mut fd);
            }
            Ok(0)
        } else {
            error!("Can't find file descriptor of {}", path);
            Err(-1)
        }
    }
    */

    pub fn file_write(&mut self, buf: &[u8]) -> Result<usize, i32> {
        let path = String::from((*self.file_path).to_str().unwrap());
        if if_cache(path.clone()) {
            // 找到 cache 直接写 cache；一旦文件膨胀到阈值以上，立即回退到底层 ext4。
            let cache = get_cache(path.clone());
            let mut cache_writer = cache.write();
            let next_size = cache_writer
                .offset
                .checked_add(buf.len())
                .ok_or(EINVAL as i32)?;
            if next_size > MAX_CACHED_FILE_SIZE {
                let write_offset = cache_writer.offset;
                drop(cache_writer);
                write_back_cache(path.clone())?;
                remove_file_cache_state(&path);
                self.file_desc.fpos = write_offset as u64;
            } else {
                cache_writer.writebuf(buf)?;
                return Ok(buf.len());
            }
        }

        let write_start = self.file_desc.fpos as usize;
        let mut rw_count = 0;
        let r = unsafe {
            ext4_fwrite(
                &mut self.file_desc,
                buf.as_ptr() as _,
                buf.len(),
                &mut rw_count,
            )
        };

        if r != EOK as i32 {
            error!("ext4_fwrite: rc = {}", r);
            return Err(r);
        }

        if write_start.saturating_add(rw_count) > MAX_CACHED_FILE_SIZE {
            self.cache_too_large = true;
        }

        //debug!("file_write {:?}, len={}", self.get_path(), rw_count);
        Ok(rw_count)
    }

    pub fn file_truncate(&mut self, size: u64) -> Result<usize, i32> {
        debug!("file_truncate to {}", size);

        let path = String::from((*self.file_path).to_str().unwrap());
        if if_cache(path.clone()) {
            let cache = get_cache(path.clone());
            let mut cache_writer = cache.write();
            cache_writer.truncate(size as usize)?;
        }

        let r = unsafe { ext4_ftruncate(&mut self.file_desc, size) };
        if r != EOK as i32 {
            error!("ext4_ftruncate: rc = {}", r);
            return Err(r);
        }
        self.cache_too_large = size > MAX_CACHED_FILE_SIZE as u64;
        Ok(EOK as usize)
    }

    pub fn file_size(&mut self) -> u64 {
        let path = String::from((*self.file_path).to_str().unwrap());
        if if_cache(path.clone()) {
            return get_cache(path.clone()).read().size as u64;
        }

        //注，记得先 O_RDONLY 打开文件
        let c_path = self.file_path.clone().into_raw();
        let c_flags = Ext4File::flags_to_cstring(2).into_raw();

        //重新打开文件获得最新的文件信息
        unsafe { ext4_fopen(&mut self.file_desc, c_path, c_flags) };
        unsafe {
            // deallocate the CString
            drop(CString::from_raw(c_path));
            drop(CString::from_raw(c_flags));
        }
        unsafe { ext4_fsize(&mut self.file_desc) }
    }

    pub fn file_cache_flush(&mut self) -> Result<usize, i32> {
        let path = String::from((*self.file_path).to_str().unwrap());
        if if_cache(path.clone()) {
            write_back_cache(path.clone())?;
        }

        let c_path = self.file_path.clone();
        let c_path = c_path.into_raw();
        unsafe {
            let r = ext4_cache_flush(c_path);
            if r != EOK as i32 {
                error!("ext4_cache_flush: rc = {}", r);
                return Err(r);
            }
            drop(CString::from_raw(c_path));
        }
        Ok(0)
    }

    pub fn set_time(
        &mut self,
        atime: Option<u64>,
        mtime: Option<u64>,
        ctime: Option<u64>,
    ) -> Result<usize, i32> {
        let c_path = self.file_path.clone();
        let c_path = c_path.into_raw();
        let mut r = 0;
        if let Some(atime) = atime {
            r = unsafe { ext4_atime_set(c_path, atime) }
        }
        if let Some(mtime) = mtime {
            r = unsafe { ext4_mtime_set(c_path, mtime) }
        }
        if let Some(ctime) = ctime {
            r = unsafe { ext4_ctime_set(c_path, ctime) }
        }
        // unsafe { ext4_mode_set(c_path, mode) };
        unsafe {
            drop(CString::from_raw(c_path));
        }
        if r != EOK as i32 {
            error!("ext4_time_set: rc = {}", r);
            return Err(r);
        }
        Ok(EOK as usize)
    }
    // Ok(atime,mtime,ctime)
    // pub fn time(&mut self) -> Result<(u32, u32, u32), i32> {
    //     let (mut atime, mut mtime, mut ctime) = (0, 0, 0);
    //     let c_path = self.file_path.clone();
    //     let c_path = c_path.into_raw();
    //     let r = unsafe {
    //         ext4_atime_get(c_path, &mut atime)
    //             | ext4_mtime_get(c_path, &mut mtime)
    //             | ext4_ctime_get(c_path, &mut ctime)
    //     };
    //     unsafe {
    //         drop(CString::from_raw(c_path));
    //     }
    //     if r != EOK as i32 {
    //         error!("ext4_mode_get: rc = {}", r);
    //         return Err(r);
    //     }
    //     Ok((atime, mtime, ctime))
    // }
    pub fn fstat(&mut self) -> Result<ext4_inode_stat, i32> {
        let path = String::from((*self.file_path).to_str().unwrap());
        let c_path = self.file_path.clone();
        let c_path = c_path.into_raw();
        let mut stat = ext4_inode_stat::default();
        let r = unsafe { ext4_stat_get(c_path, &mut stat) };

        unsafe {
            drop(CString::from_raw(c_path));
        }
        if r != EOK as i32 {
            // Small files can be visible only through the write-back cache
            // until their first flush. Keep fstat usable for that transient
            // state instead of reporting a spurious filesystem error.
            if if_cache(path.clone()) {
                let cache = get_cache(path);
                let cache = cache.read();
                stat.st_mode = cache.mode.unwrap_or(0o100000);
                stat.st_nlink = 1;
                stat.st_size = cache.size as isize;
                stat.st_blksize = 512;
                stat.st_blocks = ((cache.size + 511) / 512) as isize;
                return Ok(stat);
            }
            error!("ext4_stat_get: rc = {}", r);
            return Err(r);
        }

        if if_cache(path.clone()) {
            //如果在缓存中，更新stat获得的大小
            let cache = get_cache(path.clone());
            let cache_reader = cache.read();
            stat.st_size = cache_reader.size as isize;
            stat.st_blocks =
                (stat.st_size - 1 + (stat.st_blksize as isize)) / (stat.st_blksize as isize);
        }

        // error!(
        //     "[fstat:lwext4/src/file.rs] st_atime = {},st_ctime = {},st_mtime = {}",
        //     stat.st_atime, stat.st_ctime, stat.st_mtime
        // );
        Ok(stat)
    }

    pub fn links_cnt(&mut self) -> Result<u32, i32> {
        let mut cnt: u32 = 0;
        let c_path = self.file_path.clone();
        let c_path = c_path.into_raw();
        let r = unsafe { ext4_get_links_cnt(c_path, &mut cnt) };
        unsafe {
            drop(CString::from_raw(c_path));
        }
        if r != EOK as i32 {
            // error!("ext4_links_cnt_get: rc = {}", r);
            return Err(r);
        }
        Ok(cnt)
    }
    pub fn file_mode(&mut self) -> Result<u32, i32> {
        // 0o777 (octal) == rwxrwxrwx
        let mut mode: u32 = 0o777;
        let c_path = self.file_path.clone();
        let c_path = c_path.into_raw();
        let r = unsafe { ext4_mode_get(c_path, &mut mode) };
        unsafe {
            drop(CString::from_raw(c_path));
        }
        if r != EOK as i32 {
            error!("ext4_mode_get: rc = {}", r);
            return Err(r);
        }
        //debug!("Got file mode={:#x}", mode);
        Ok(mode)
    }

    pub fn file_mode_set(&mut self, mode: u32) -> Result<usize, i32> {
        //debug!("file_mode_set to {:#x}", mode);

        let c_path = self.file_path.clone();
        let c_path = c_path.into_raw();
        let r = unsafe { ext4_mode_set(c_path, mode) };
        unsafe {
            drop(CString::from_raw(c_path));
        }
        if r != EOK as i32 {
            error!("ext4_mode_set: rc = {}", r);
            return Err(r);
        }
        self.pending_mode = Some(mode);
        let path = String::from((*self.file_path).to_str().unwrap());
        if if_cache(path.clone()) {
            get_cache(path).write().mode = Some(mode);
        }
        Ok(EOK as usize)
    }

    pub fn file_owner_set(&mut self, uid: u32, gid: u32) -> Result<usize, i32> {
        // chown/fchownat need the on-disk inode owner, not just cached stat data.
        let c_path = self.file_path.clone();
        let c_path = c_path.into_raw();
        let r = unsafe { ext4_owner_set(c_path, uid, gid) };
        unsafe {
            drop(CString::from_raw(c_path));
        }
        if r != EOK as i32 {
            error!("ext4_owner_set: rc = {}", r);
            return Err(r);
        }
        Ok(EOK as usize)
    }

    pub fn file_type(&mut self) -> InodeTypes {
        let mode = self.file_mode();
        if mode.is_err() {
            warn!("file_type mode.is_err");
            return InodeTypes::EXT4_INODE_MODE_FILE;
        }
        let mode = mode.unwrap();
        let types = mode & 0o170000;
        let itypes = match types {
            0x1000 => InodeTypes::EXT4_INODE_MODE_FIFO,
            0x2000 => InodeTypes::EXT4_INODE_MODE_CHARDEV,
            0x4000 => InodeTypes::EXT4_INODE_MODE_DIRECTORY,
            0x6000 => InodeTypes::EXT4_INODE_MODE_BLOCKDEV,
            0x8000 => InodeTypes::EXT4_INODE_MODE_FILE,
            0xA000 => InodeTypes::EXT4_INODE_MODE_SOFTLINK,
            0xC000 => InodeTypes::EXT4_INODE_MODE_SOCKET,
            0xF000 => InodeTypes::EXT4_INODE_MODE_TYPE_MASK,
            _ => {
                warn!("Unknown inode mode type {:x}", types);
                InodeTypes::EXT4_INODE_MODE_FILE
            }
        };
        //debug!("Inode mode types: {:?}", itypes);

        itypes
    }

    /********* DIRECTORY OPERATION *********/

    /// Create new directory
    pub fn dir_mk(&mut self, path: &str) -> Result<usize, i32> {
        //debug!("directory create: {}", path);
        let c_path = CString::new(path).expect("CString::new failed");
        let c_path = c_path.into_raw();

        let r = unsafe { ext4_dir_mk(c_path) };
        unsafe {
            drop(CString::from_raw(c_path));
        }
        if r != EOK as i32 {
            error!("ext4_dir_mk: rc = {}", r);
            return Err(r);
        }
        Ok(EOK as usize)
    }

    /// Rename/move directory
    pub fn dir_mv(&mut self, path: &str, new_path: &str) -> Result<usize, i32> {
        //debug!("directory move from {} to {}", path, new_path);

        let c_path = CString::new(path).expect("CString::new failed");
        let c_path = c_path.into_raw();
        let c_new_path = CString::new(new_path).expect("CString::new failed");
        let c_new_path = c_new_path.into_raw();

        let r = unsafe { ext4_dir_mv(c_path, c_new_path) };
        unsafe {
            drop(CString::from_raw(c_path));
            drop(CString::from_raw(c_new_path));
        }
        if r != EOK as i32 {
            error!("ext4_dir_mv: rc = {}", r);
            return Err(r);
        }
        Ok(EOK as usize)
    }

    /// Recursive directory remove
    pub fn dir_rm(&mut self, path: &str) -> Result<usize, i32> {
        //debug!("directory recursive remove: {}", path);

        let c_path = CString::new(path).expect("CString::new failed");
        let c_path = c_path.into_raw();

        let r = unsafe { ext4_dir_rm(c_path) };
        unsafe {
            drop(CString::from_raw(c_path));
        }
        if (r != EOK as i32) && (r != ENOENT as i32) {
            error!("ext4_fremove ext4_dir_rm: rc = {}", r);
            return Err(r);
        }
        Ok(EOK as usize)
    }

    pub fn read_dir_from(&self, off: u64) -> Result<Vec<OsDirent>, i32> {
        if self.this_type != InodeTypes::EXT4_DE_DIR {
            return Err(22);
        }
        let c_path = self.file_path.clone();
        let c_path = c_path.into_raw();
        let mut d: ext4_dir = unsafe { core::mem::zeroed() };
        let mut entries: Vec<_> = Vec::new();

        unsafe {
            ext4_dir_open(&mut d, c_path);
            drop(CString::from_raw(c_path));
            d.next_off = off;
            let mut de = ext4_dir_entry_next(&mut d);
            while !de.is_null() {
                let dentry = &(*de);
                //对齐 align8
                let mut name = [0u8; 256];
                let name_len = dentry.name_length as usize;
                name[0..name_len].copy_from_slice(&dentry.name[0..name_len]);
                let mut len = name_len + 19;
                let align = 8 - len % 8;
                len += align;
                entries.push(OsDirent {
                    d_ino: dentry.inode as u64,
                    d_off: d.next_off as i64,
                    d_reclen: len as u16,
                    d_type: ext4_dirent_type_to_linux_dtype(dentry.inode_type),
                    d_name: name,
                });
                de = ext4_dir_entry_next(&mut d);
            }
            ext4_dir_close(&mut d);
        }
        Ok(entries)
    }
}

/*
pub enum OpenFlags {
O_RDONLY = 0,
O_WRONLY = 0x1,
O_RDWR = 0x2,
O_CREAT = 0x40,
O_TRUNC = 0x200,
O_APPEND = 0x400,
}
*/

#[derive(PartialEq, Clone, Debug)]
pub enum InodeTypes {
    // Inode type, Directory entry types.
    EXT4_DE_UNKNOWN = 0,
    EXT4_DE_REG_FILE = 1,
    EXT4_DE_DIR = 2,
    EXT4_DE_CHRDEV = 3,
    EXT4_DE_BLKDEV = 4,
    EXT4_DE_FIFO = 5,
    EXT4_DE_SOCK = 6,
    EXT4_DE_SYMLINK = 7,

    // Inode mode
    EXT4_INODE_MODE_FIFO = 0x1000,
    EXT4_INODE_MODE_CHARDEV = 0x2000,
    EXT4_INODE_MODE_DIRECTORY = 0x4000,
    EXT4_INODE_MODE_BLOCKDEV = 0x6000,
    EXT4_INODE_MODE_FILE = 0x8000,
    EXT4_INODE_MODE_SOFTLINK = 0xA000,
    EXT4_INODE_MODE_SOCKET = 0xC000,
    EXT4_INODE_MODE_TYPE_MASK = 0xF000,
}

impl From<usize> for InodeTypes {
    fn from(num: usize) -> InodeTypes {
        match num {
            0 => InodeTypes::EXT4_DE_UNKNOWN,
            1 => InodeTypes::EXT4_DE_REG_FILE,
            2 => InodeTypes::EXT4_DE_DIR,
            3 => InodeTypes::EXT4_DE_CHRDEV,
            4 => InodeTypes::EXT4_DE_BLKDEV,
            5 => InodeTypes::EXT4_DE_FIFO,
            6 => InodeTypes::EXT4_DE_SOCK,
            7 => InodeTypes::EXT4_DE_SYMLINK,
            0x1000 => InodeTypes::EXT4_INODE_MODE_FIFO,
            0x2000 => InodeTypes::EXT4_INODE_MODE_CHARDEV,
            0x4000 => InodeTypes::EXT4_INODE_MODE_DIRECTORY,
            0x6000 => InodeTypes::EXT4_INODE_MODE_BLOCKDEV,
            0x8000 => InodeTypes::EXT4_INODE_MODE_FILE,
            0xA000 => InodeTypes::EXT4_INODE_MODE_SOFTLINK,
            0xC000 => InodeTypes::EXT4_INODE_MODE_SOCKET,
            0xF000 => InodeTypes::EXT4_INODE_MODE_TYPE_MASK,
            _ => {
                warn!("Unknown ext4 inode type: {}", num);
                InodeTypes::EXT4_DE_UNKNOWN
            }
        }
    }
}
#[repr(C)]
#[derive(Debug)]
pub struct OsDirent {
    pub d_ino: u64,        // 索引节点号
    pub d_off: i64,        // 从 0 开始到下一个 dirent 的偏移
    pub d_reclen: u16,     // 当前 dirent 的长度
    pub d_type: u8,        // 文件类型
    pub d_name: [u8; 256], // 文件名
}

impl OsDirent {
    #[inline(always)]
    pub fn len(&self) -> usize {
        self.d_reclen as usize
    }
    #[inline(always)]
    pub fn off(&self) -> usize {
        self.d_off as usize
    }

    /// 将当前目录项解释为字节切片，用于 getdents64 序列化
    pub fn as_bytes(&self) -> &[u8] {
        // 名字数组大小不定，按 d_reclen 解释
        unsafe { core::slice::from_raw_parts(self as *const _ as *const u8, self.len()) }
    }
}

#[derive(Clone)]
pub struct VFileCache {
    data: Vec<u8>,
    offset: usize,
    modified: bool,
    size: usize,
    mode: Option<u32>,
}

impl VFileCache {
    pub fn new() -> Self {
        Self {
            data: Vec::new(),
            offset: 0,
            modified: false,
            size: 0,
            mode: None,
        }
    }

    pub fn get_data_slice(&self) -> &[u8] {
        &self.data.as_slice()[..]
    }

    pub fn writebuf(&mut self, buf: &[u8]) -> Result<usize, i32> {
        let length = buf.len();
        let end = self.offset.checked_add(length).ok_or(EINVAL as i32)?;
        if end > self.size {
            self.size = end;
        }
        if end > self.data.len() {
            let aligned_size = aligned_down(end)
                .checked_add(PAGE_SIZE)
                .ok_or(ENOMEM as i32)?;
            let additional = aligned_size.saturating_sub(self.data.capacity());
            if additional > 0 {
                self.data
                    .try_reserve_exact(additional)
                    .map_err(|_| ENOMEM as i32)?;
            }
            self.data.resize(aligned_size, 0);
        }
        if length <= 10 {
            for i in 0..length {
                self.data[self.offset + i] = buf[i];
            }
        } else {
            self.data[self.offset..end].copy_from_slice(buf);
        }
        self.modified = true;
        /*
        debug!(
            "write {} bytes and size is {}, data.len() is {} now",
            length,
            self.size,
            self.data.len()
        );
        */
        Ok(self.data.len())
    }

    pub fn truncate(&mut self, new_size: usize) -> Result<(), i32> {
        let aligned_size = aligned_down(new_size) + PAGE_SIZE;
        let additional = aligned_size.saturating_sub(self.data.capacity());
        if additional > 0 {
            self.data
                .try_reserve_exact(additional)
                .map_err(|_| ENOMEM as i32)?;
        }
        self.data.resize(aligned_size, 0);
        let length = self.data.len();
        if new_size < length {
            self.data[new_size..length].fill(0);
        }
        self.size = new_size;
        Ok(())
    }
}

//cache表，目前只为非目录文件使用cache
static CACHE_TABLE: Lazy<Mutex<BTreeMap<String, Arc<RwLock<VFileCache>>>>> =
    Lazy::new(|| Mutex::new(BTreeMap::new()));

pub fn if_cache(file_path: String) -> bool {
    CACHE_TABLE.lock().contains_key(&file_path)
}

pub fn get_cache(file_path: String) -> Arc<RwLock<VFileCache>> {
    CACHE_TABLE.lock().get(&file_path).unwrap().clone()
}

pub fn insert_cache(file_path: String, cache: &Arc<RwLock<VFileCache>>) {
    CACHE_TABLE.lock().insert(file_path, cache.clone());
}

pub fn remove_cache(file_path: String) {
    CACHE_TABLE.lock().remove(&file_path);
}

/// Remove every global write-back bookkeeping entry for a pathname.
///
/// The guards are deliberately released between tables so cache removal never
/// waits for FIFO state while retaining a cache-table guard.
fn remove_file_cache_state(file_path: &str) {
    {
        let mut fifo = FIFO_TABLE.lock();
        fifo.retain(|entry| entry != file_path);
    }
    CACHE_TABLE.lock().remove(file_path);
}

fn is_proc_task_runtime_file(path: &str) -> bool {
    let rest = match path.strip_prefix("/proc/") {
        Some(rest) => rest,
        None => return false,
    };
    let (pid, name) = match rest.split_once('/') {
        Some(parts) => parts,
        None => return false,
    };

    !pid.is_empty()
        && pid.as_bytes().iter().all(|b| matches!(*b, b'0'..=b'9'))
        && matches!(name, "stat" | "status" | "maps")
}

const FIFO_SIZE: usize = 10;
//采用先进先出策略
static FIFO_TABLE: Lazy<Mutex<VecDeque<String>>> = Lazy::new(|| Mutex::new(VecDeque::new()));

pub fn insert_fifo(file_path: String) {
    let evicted = {
        let mut fifo = FIFO_TABLE.lock();
        // FIFO_SIZE is intentionally small, so keeping de-duplication in the
        // queue itself avoids a second table with a separate lifetime.
        if fifo.iter().any(|entry| entry == &file_path) {
            return;
        }

        let evicted = if fifo.len() == FIFO_SIZE {
            let path = fifo.pop_front().unwrap();
            // Detach the global state before doing lwext4 I/O.  A pathname
            // recreated during the write-back can then receive a new cache.
            CACHE_TABLE.lock().remove(&path).map(|cache| (path, cache))
        } else {
            None
        };

        fifo.push_back(file_path.clone());
        evicted
    };

    // write_back_cache_entry() may handle an ENOENT proc entry.  It must run
    // after FIFO_TABLE is unlocked because it can otherwise re-enter cache
    // cleanup paths.
    if let Some((path, cache)) = evicted {
        let _ = write_back_cache_entry(&path, &cache);
    }
}

pub fn write_back_cache(path: String) -> Result<usize, i32> {
    let cache = CACHE_TABLE.lock().get(&path).cloned();
    match cache {
        Some(cache) => write_back_cache_entry(&path, &cache),
        None => Ok(0),
    }
}

fn write_back_cache_entry(path: &str, cache: &Arc<RwLock<VFileCache>>) -> Result<usize, i32> {
    let cache_writer = cache.write();
    if !cache_writer.modified {
        return Ok(0);
    }

    //如果被修改过，则写回
    // debug!("{} is written back!", path);
    let c_path = CString::new(path).expect("CString::new failed");
    let c_path = c_path.into_raw();
    let flags = Ext4File::flags_to_cstring(2).into_raw();
    let mut file_desc = ext4_file {
        mp: core::ptr::null_mut(),
        inode: 0,
        flags: 0,
        fsize: 0,
        fpos: 0,
    };
    let mut r = unsafe { ext4_fopen(&mut file_desc, c_path, flags) };
    unsafe {
        // deallocate the CString
        drop(CString::from_raw(c_path));
        drop(CString::from_raw(flags));
    }
    if r == ENOENT as i32 {
        // Runtime proc files are removed together with their task.
        // They must never be recreated from a stale write-back cache.
        if is_proc_task_runtime_file(path) {
            return Ok(0);
        }
        // A newly created file can remain only in the write-back cache
        // until its first eviction. Materialize that cache entry and
        // then flush the pending contents through the same descriptor.
        file_desc = ext4_file {
            mp: core::ptr::null_mut(),
            inode: 0,
            flags: 0,
            fsize: 0,
            fpos: 0,
        };
        let c_path = CString::new(path).expect("CString::new failed");
        let c_path = c_path.into_raw();
        let flags = Ext4File::flags_to_cstring(O_RDWR | O_CREAT | O_TRUNC).into_raw();
        r = unsafe { ext4_fopen(&mut file_desc, c_path, flags) };
        unsafe {
            drop(CString::from_raw(c_path));
            drop(CString::from_raw(flags));
        }
        if r == EOK as i32 {
            if let Some(mode) = cache_writer.mode {
                let c_path = CString::new(path).expect("CString::new failed");
                let c_path = c_path.into_raw();
                r = unsafe { ext4_mode_set(c_path, mode) };
                unsafe {
                    drop(CString::from_raw(c_path));
                }
            }
        }
    }
    if r != EOK as i32 {
        error!("write_back_cache ext4_fopen: {}, rc = {}", path, r);
        return Err(r);
    }

    file_desc.fpos = 0;
    let mut rw_count = 0;
    let r = unsafe {
        ext4_fwrite(
            &mut file_desc,
            cache_writer.data.as_ptr() as _,
            cache_writer.size,
            &mut rw_count,
        )
    };
    if r != EOK as i32 {
        error!("write_back_cache ext4_fwrite: {}, rc = {}", path, r);
        unsafe {
            ext4_fclose(&mut file_desc);
        }
        return Err(r);
    }
    let r = unsafe { ext4_fclose(&mut file_desc) };
    if r != EOK as i32 {
        error!("write_back_cache ext4_fclose: {}, rc = {}", path, r);
        return Err(r);
    }
    Ok(rw_count)
}

fn seek_pos(current: usize, size: usize, offset: i64, seek_type: u32) -> Result<usize, i32> {
    match seek_type {
        SEEK_SET => {
            if offset < 0 {
                Err(EINVAL as i32)
            } else {
                Ok(offset as usize)
            }
        }
        SEEK_CUR => {
            if offset < 0 {
                current.checked_sub((-offset) as usize).ok_or(EINVAL as i32)
            } else {
                current.checked_add(offset as usize).ok_or(EINVAL as i32)
            }
        }
        SEEK_END => {
            if offset < 0 {
                size.checked_sub((-offset) as usize).ok_or(EINVAL as i32)
            } else {
                size.checked_add(offset as usize).ok_or(EINVAL as i32)
            }
        }
        _ => Err(EINVAL as i32),
    }
}
