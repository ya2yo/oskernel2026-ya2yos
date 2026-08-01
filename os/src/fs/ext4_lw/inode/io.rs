//! File data operations for the lwext4 inode adapter.

use super::*;

impl Ext4Inode {
    /// 获取普通文件大小。
    ///
    /// 目录和其他非普通文件当前返回 0；普通文件需要按 lwext4 API 重新打开后读取 size。
    pub(super) fn size_impl(&self) -> usize {
        if self.inode_type != InodeType::File {
            return 0;
        }
        if let Some(size) = self.known_size() {
            return size;
        }

        let _io_state = self.io_state.lock();
        let _ext4 = EXT4_OP_LOCK.lock_for_metadata();
        #[cfg(feature = "perf")]
        let _phase = Ext4InodePhaseGuard::metadata(Ext4MetadataPhase::Size);
        let inner = self.inner.get_unchecked_mut();
        if let Some(size) = self.known_size() {
            return size;
        }
        let path = Self::live_path(inner);
        let file = &mut inner.f;
        file.file_open_read_only(&path);
        let size = file.file_size() as usize;
        self.update_known_size(size);
        size
    }
    /// 从指定偏移量读取数据到缓冲区。
    ///
    /// 动态链接文件可能需要按路径 patch 内容，因此读取完成后会调用
    /// `patch_dynamic_link_file_bytes()` 做兼容修补。
    pub(super) fn read_at_impl(&self, off: usize, buf: &mut [u8]) -> SyscallRet {
        if buf.is_empty() {
            return Ok(0);
        }
        // The delayed byte cache has its own synchronization and contains
        // every visible byte for its dense, non-sparse inode.  Check it
        // before taking the mount-wide lwext4 guard: concurrent compiler
        // readers otherwise queue behind unrelated block I/O merely to copy
        // data that is already resident in kernel memory.
        let cached_path = self.cached_page_cache_path();
        if let Some(r) = read_cached_at(&cached_path, off, buf) {
            #[cfg(feature = "perf")]
            {
                crate::utils::perf::record_ext4_read(r);
                crate::utils::perf::record_ext4_byte_cache_read_hit(r);
            }
            patch_dynamic_link_file_bytes(&cached_path, off, &mut buf[..r]);
            return Ok(r);
        }
        // Keep this inode's descriptor stable across the two serialized lwext4
        // calls, but release the mount-wide gate after `open`. A different
        // inode can use lwext4 while this reader waits to acquire the data
        // section, whereas the old single critical section kept it blocked.
        // The compatibility patch below only mutates an already-read buffer.
        let (path, r, byte_cache_hit) = {
            let _io_state = self.io_state.lock();
            let path = self.cached_path();
            // Read-back caches may contain dirty bytes which are not on disk yet.
            // Recheck after obtaining the inode lock in case a previous writer
            // initialized this pathname's cache after the optimistic lookup.
            let (r, byte_cache_hit) = if let Some(r) = read_cached_at(&path, off, buf) {
                (r, true)
            } else {
                let needs_open = !self.inner.get_unchecked_mut().f.is_open_for_read(&path);
                if needs_open {
                    let _ext4 = EXT4_OP_LOCK.lock_for_read_open();
                    self.inner
                        .get_unchecked_mut()
                        .f
                        .file_open_read_only(&path)
                        .map_err(SysErrNo::from)?;
                }
                let r = {
                    let _ext4 = EXT4_OP_LOCK.lock_for_read_data();
                    self.inner
                        .get_unchecked_mut()
                        .f
                        .file_read_at(off, buf)
                        .map_err(SysErrNo::from)?
                };
                (r, false)
            };
            (path, r, byte_cache_hit)
        };
        #[cfg(not(feature = "perf"))]
        let _ = byte_cache_hit;
        #[cfg(feature = "perf")]
        {
            crate::utils::perf::record_ext4_read(r);
            if byte_cache_hit {
                crate::utils::perf::record_ext4_byte_cache_read_hit(r);
            }
        }
        // lwext4's ext4_fread() only reads blocks and advances the descriptor
        // position; atime changes go through the explicit set_timestamps()
        // path. Keeping the immutable regular-file stat cache here avoids
        // turning the next fstat() into another serialized metadata lookup.
        patch_dynamic_link_file_bytes(&path, off, &mut buf[..r]);
        Ok(r)
    }

    /// 从指定偏移量写入数据。
    pub(super) fn write_at_impl(&self, off: usize, buf: &[u8]) -> SyscallRet {
        // The state lock keeps the path, cache policy and quota reservation
        // stable while an in-memory write bypasses `EXT4_OP_LOCK`.  Slow paths
        // retain the original global serialization before entering lwext4.
        let _write_state = self.write_state.lock();
        let _io_state = self.io_state.lock();
        let end = off.checked_add(buf.len()).ok_or(SysErrNo::EFBIG)?;
        let cached_path = self.cached_path();
        if !self.delayed.load(Ordering::Acquire)
            && end <= self.quota_reserved.load(Ordering::Acquire)
        {
            if let Some(current_size) = self.known_size() {
                if let Some(result) = write_cached_at(cached_path.as_ref(), off, buf) {
                    let written = result.map_err(SysErrNo::from)?;
                    let end = off.checked_add(written).ok_or(SysErrNo::EFBIG)?;
                    self.update_known_size(current_size.max(end));
                    self.invalidate_cached_stat(Ext4FstatMissReason::DenseWriteBack);
                    return Ok(written);
                }
            }
        }

        let (path, current_size) = {
            let _ext4 = EXT4_OP_LOCK.lock_for_write_open();
            let inner = self.inner.get_unchecked_mut();
            let path = Self::live_path(inner);
            let delayed = inner.delay;
            let file = &mut inner.f;
            #[cfg(feature = "perf")]
            let open_phase = crate::utils::perf::Ext4WritePhaseGuard::new(
                crate::utils::perf::Ext4WritePhase::Open,
            );
            file.ensure_open(O_RDWR).map_err(SysErrNo::from)?;
            #[cfg(feature = "perf")]
            drop(open_phase);
            if delayed {
                // Keep an unlinked-but-open temporary file's cache alive until
                // its last fd closes; the FIFO cannot otherwise distinguish it
                // from an idle cache and will repeatedly evict/rebuild it.
                file.pin_write_back_cache();
            }
            let current_size = self
                .known_size()
                .unwrap_or_else(|| file.file_size() as usize);
            (path, current_size)
        };
        #[cfg(feature = "perf")]
        let quota_phase =
            crate::utils::perf::Ext4WritePhaseGuard::new(crate::utils::perf::Ext4WritePhase::Quota);
        let previous_reserved = self.quota_reserved.load(Ordering::Acquire);
        let reservation = if end > previous_reserved {
            let target = if end <= QUOTA_RESERVE_GRANULARITY {
                end
            } else {
                end.checked_add(QUOTA_RESERVE_GRANULARITY - 1)
                    .map(|value| value / QUOTA_RESERVE_GRANULARITY * QUOTA_RESERVE_GRANULARITY)
                    .ok_or(SysErrNo::EFBIG)?
            };
            let previous = previous_reserved.max(current_size);
            let mut mount_table = MNT_TABLE.lock();
            if let Err(error) = mount_table.reserve_write(&path, previous, target) {
                // The final chunk may be smaller than the reservation unit.
                // Charge only this write before reporting ENOSPC so callers
                // can consume the mount exactly up to its real limit.
                if target == end || error != SysErrNo::ENOSPC {
                    Err(error)
                } else if let Err(error) = mount_table.reserve_write(&path, previous, end) {
                    Err(error)
                } else {
                    self.quota_reserved.store(end, Ordering::Release);
                    Ok(Some((previous, end)))
                }
            } else {
                self.quota_reserved.store(target, Ordering::Release);
                Ok(Some((previous, target)))
            }
        } else {
            Ok(None)
        };
        #[cfg(feature = "perf")]
        drop(quota_phase);
        let reservation = match reservation {
            Ok(reservation) => reservation,
            Err(error) => {
                if error == SysErrNo::ENOSPC {
                    let _ext4 = EXT4_OP_LOCK.lock_for_write_data();
                    self.inner.get_unchecked_mut().f.defer_close_flush();
                }
                return Err(error);
            }
        };
        #[cfg(feature = "perf")]
        let data_phase =
            crate::utils::perf::Ext4WritePhaseGuard::new(crate::utils::perf::Ext4WritePhase::Data);
        let write_result = {
            let _ext4 = EXT4_OP_LOCK.lock_for_write_data();
            let file = &mut self.inner.get_unchecked_mut().f;
            let result = if off > current_size {
                // A write beyond EOF creates a sparse range. The whole-file cache
                // tracks bytes only and would otherwise materialize that range.
                file.disable_write_back_cache()
                    .map_err(SysErrNo::from)
                    .and_then(|_| file.file_write_at(off, buf).map_err(SysErrNo::from))
            } else {
                file.file_write_at(off, buf).map_err(SysErrNo::from)
            };
            match result {
                Ok(written) => {
                    #[cfg(feature = "perf")]
                    let fstat_miss_reason = match file.last_write_path() {
                        lwext4_rust::perf::FileWritePath::DenseWriteBack => {
                            Ext4FstatMissReason::DenseWriteBack
                        }
                        lwext4_rust::perf::FileWritePath::Direct => {
                            Ext4FstatMissReason::DirectWrite
                        }
                        lwext4_rust::perf::FileWritePath::SparseBuffered => {
                            Ext4FstatMissReason::SparseBufferedWrite
                        }
                    };
                    #[cfg(not(feature = "perf"))]
                    let fstat_miss_reason = Ext4FstatMissReason::DirectWrite;
                    Ok((written, fstat_miss_reason))
                }
                Err(error) => {
                    if error == SysErrNo::ENOSPC {
                        file.defer_close_flush();
                    }
                    Err(error)
                }
            }
        };
        #[cfg(feature = "perf")]
        drop(data_phase);
        let (written, fstat_miss_reason) = match write_result {
            Ok(result) => result,
            Err(error) => {
                if let Some((previous, target)) = reservation {
                    MNT_TABLE
                        .lock()
                        .rollback_reservation(&path, previous, target);
                    self.quota_reserved
                        .store(previous_reserved, Ordering::Release);
                }
                return Err(error);
            }
        };
        let end = off.checked_add(written).ok_or(SysErrNo::EFBIG)?;
        self.update_known_size(current_size.max(end));
        self.invalidate_cached_stat(fstat_miss_reason);
        Ok(written)
    }

    /// 截断文件到指定长度。
    ///
    /// 成功后失效文件页缓存，避免 mmap/page cache 继续暴露旧大小或旧内容。
    pub(super) fn truncate_impl(&self, size: usize) -> SyscallRet {
        let _write_state = self.write_state.lock();
        let _io_state = self.io_state.lock();
        let _ext4 = EXT4_OP_LOCK.lock_for_namespace();
        #[cfg(feature = "perf")]
        let _phase = Ext4InodePhaseGuard::namespace(Ext4NamespacePhase::Truncate);
        let inner = self.inner.get_unchecked_mut();
        let path = Self::live_path(inner);
        let file = &mut inner.f;
        file.file_open(&path, O_RDWR).map_err(SysErrNo::from)?;

        file.file_truncate(size as u64).map_err(SysErrNo::from)?;
        self.update_known_size(size);
        self.invalidate_cached_stat(Ext4FstatMissReason::Truncate);
        FILE_PAGE_CACHE.invalidate_path(&path);
        Ok(0)
    }
    /// 将 lwext4 文件缓存刷新到磁盘。
    pub(super) fn sync_impl(&self) {
        let _io_state = self.io_state.lock();
        let _ext4 = EXT4_OP_LOCK.lock_for_sync();
        let inner = self.inner.get_unchecked_mut();
        let _ = Self::live_path(inner);
        inner.f.file_cache_flush();
    }

    /// 一次性读取整个文件内容。
    ///
    /// 普通文件直接读取全部字节；符号链接会先解析链接目标，再递归读取真实文件。
    pub(super) fn read_all_impl(&self) -> Result<Vec<u8>, SysErrNo> {
        // 先提取 path 和类型，避免后续访问 self.inner 时产生重叠借用
        let (file_type, path_str) = {
            let _io_state = self.io_state.lock();
            let _ext4 = EXT4_OP_LOCK.lock_for_metadata();
            #[cfg(feature = "perf")]
            let _phase = Ext4InodePhaseGuard::metadata(Ext4MetadataPhase::ReadAllPrepare);
            let inner = self.inner.get_unchecked_mut();
            let path = Self::live_path(inner);
            let file_type = as_inode_type(inner.f.types());
            (file_type, path)
        };

        if file_type == InodeType::File {
            let _io_state = self.io_state.lock();
            let _ext4 = EXT4_OP_LOCK.lock_for_read_all();
            let file = &mut self.inner.get_unchecked_mut().f;
            file.file_open_read_only(&path_str)
                .map_err(SysErrNo::from)?;
            let size = file.file_size() as usize;
            self.update_known_size(size);
            let mut buf: Vec<u8> = vec![0; size];
            let r = if let Some(r) = read_cached_at(&path_str, 0, buf.as_mut_slice()) {
                Ok(r)
            } else {
                file.file_read_at(0, buf.as_mut_slice())
            };
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
            link_file.read_link_impl(&mut real_path_buf, 256)?;
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
            let real_file = self.find_impl(&next_path, OpenFlags::O_RDONLY, 0)?;
            real_file.read_all()
        } else {
            // 目录或其他不支持的类型
            Err(SysErrNo::EISDIR)
        }
    }
    pub(super) fn seek_data_impl(&self, offset: usize) -> SyscallRet {
        let _write_state = self.write_state.lock();
        let _io_state = self.io_state.lock();
        let _ext4 = EXT4_OP_LOCK.lock_for_seek();
        let inner = self.inner.get_unchecked_mut();
        let path = Self::live_path(inner);
        let file = &mut inner.f;
        file.file_open(&path, O_RDONLY).map_err(SysErrNo::from)?;
        // Opening first establishes the stable `(mountpoint, inode)` cache
        // key.  Disabling before that would only affect this descriptor and
        // let another alias recreate a byte-only cache for the sparse inode.
        file.disable_write_back_cache().map_err(SysErrNo::from)?;
        match file.file_seek_data(offset as u64) {
            Ok(pos) => Ok(pos as usize),
            Err(rc) => Err(SysErrNo::from(rc)),
        }
    }

    pub(super) fn seek_hole_impl(&self, offset: usize) -> SyscallRet {
        let _write_state = self.write_state.lock();
        let _io_state = self.io_state.lock();
        let _ext4 = EXT4_OP_LOCK.lock_for_seek();
        let inner = self.inner.get_unchecked_mut();
        let path = Self::live_path(inner);
        let file = &mut inner.f;
        file.file_open(&path, O_RDONLY).map_err(SysErrNo::from)?;
        // See `seek_data()`: the cache policy is inode-wide, so the
        // descriptor must be open before deriving its key.
        file.disable_write_back_cache().map_err(SysErrNo::from)?;
        match file.file_seek_hole(offset as u64) {
            Ok(pos) => Ok(pos as usize),
            Err(rc) => Err(SysErrNo::from(rc)),
        }
    }
}
