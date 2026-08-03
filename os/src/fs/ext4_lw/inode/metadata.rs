//! Metadata and directory operations for the lwext4 inode adapter.

use super::*;

impl Ext4Inode {
    pub(super) fn set_xattr_impl(&self, name: &[u8], value: &[u8], flags: u32) -> SyscallRet {
        const XATTR_CREATE: u32 = 0x1;
        const XATTR_REPLACE: u32 = 0x2;

        if flags & !(XATTR_CREATE | XATTR_REPLACE) != 0 {
            return Err(SysErrNo::EINVAL);
        }

        // The existence check required for CREATE/REPLACE and the update is
        // kept under this inode's VFS state.  lwext4 serializes the matching
        // inode and transaction resources underneath it.
        let _write_state = self.write_state.lock();
        let _io_state = self.io_state.lock();
        let inner = self.inner.get_unchecked_mut();
        let path = Self::live_path(inner);

        if flags != 0 {
            let mut ignored = [];
            match inner.f.xattr_get(&path, name, &mut ignored) {
                Ok(_) if flags & XATTR_CREATE != 0 => return Err(SysErrNo::EEXIST),
                Ok(_) => {}
                Err(rc) if SysErrNo::from(rc) == SysErrNo::ENODATA => {
                    if flags & XATTR_REPLACE != 0 {
                        return Err(SysErrNo::ENODATA);
                    }
                }
                Err(rc) => return Err(SysErrNo::from(rc)),
            }
        }

        inner
            .f
            .xattr_set(&path, name, value)
            .map_err(SysErrNo::from)
    }

    pub(super) fn get_xattr_impl(&self, name: &[u8], value: &mut [u8]) -> SyscallRet {
        let _io_state = self.io_state.lock();
        let inner = self.inner.get_unchecked_mut();
        let path = Self::live_path(inner);
        inner
            .f
            .xattr_get(&path, name, value)
            .map_err(SysErrNo::from)
    }

    pub(super) fn list_xattr_impl(&self, list: &mut [u8]) -> SyscallRet {
        let _io_state = self.io_state.lock();
        let inner = self.inner.get_unchecked_mut();
        let path = Self::live_path(inner);
        inner.f.xattr_list(&path, list).map_err(SysErrNo::from)
    }

    pub(super) fn remove_xattr_impl(&self, name: &[u8]) -> SyscallRet {
        let _write_state = self.write_state.lock();
        let _io_state = self.io_state.lock();
        let inner = self.inner.get_unchecked_mut();
        let path = Self::live_path(inner);
        inner.f.xattr_remove(&path, name).map_err(SysErrNo::from)
    }

    /// 设置 inode 时间戳。
    pub(super) fn set_timestamps_impl(
        &self,
        atime: Option<u64>,
        mtime: Option<u64>,
        ctime: Option<u64>,
    ) -> SyscallRet {
        let _io_state = self.io_state.lock();
        #[cfg(feature = "perf")]
        let _phase = Ext4InodePhaseGuard::metadata(Ext4MetadataPhase::Timestamp);
        let inner = self.inner.get_unchecked_mut();
        let _ = Self::live_path(inner);
        let file = &mut inner.f;
        let ret = file.set_time(atime, mtime, ctime).map_err(SysErrNo::from);
        if ret.is_ok() {
            if self.inode_type == InodeType::Dir {
                self.advance_local_directory_stat_epoch();
            }
            self.invalidate_cached_stat(Ext4FstatMissReason::Metadata);
        }
        ret
    }

    /// 获取文件状态信息。
    ///
    /// 正常情况下直接用当前路径对应的 lwext4 句柄查询；如果路径因 rename/unlink 失效，
    /// 再尝试 `recover_live_path()`，从已记录 alias 中恢复一个仍存在的路径。
    pub(super) fn fstat_impl(&self) -> Kstat {
        #[cfg(feature = "perf")]
        let fstat_path = Ext4FstatPathGuard::new();
        if let Some(stat) = self.cached_stat() {
            #[cfg(feature = "perf")]
            fstat_path.finish(Ext4FstatPath::FastCached);
            return self.stat_with_known_size(stat);
        }
        if let Some(stat) = self.cached_directory_stat() {
            #[cfg(feature = "perf")]
            fstat_path.finish(Ext4FstatPath::DirectoryEpochCached);
            return stat;
        }

        let _write_state = self.write_state.lock();
        let _io_state = self.io_state.lock();
        // A failed pathname lookup recovers through `file_close()`, which may
        // publish delayed data.  This inode's VFS state protects the complete
        // sequence, while lwext4 takes its namespace/inode resources.
        // Another hart may have populated the cache while this task waited
        // for this inode. Recheck before doing redundant
        // metadata I/O during Cargo's parallel probes.
        if let Some(stat) = self.cached_stat() {
            #[cfg(feature = "perf")]
            fstat_path.finish(Ext4FstatPath::PostWaitCached);
            return self.stat_with_known_size(stat);
        }
        if let Some(stat) = self.cached_directory_stat() {
            #[cfg(feature = "perf")]
            fstat_path.finish(Ext4FstatPath::PostWaitDirectoryCached);
            return stat;
        }
        let inner = self.inner.get_unchecked_mut();
        #[cfg(feature = "perf")]
        let miss_reason = self.stat_cache_miss_reason();
        let fstat_result = {
            #[cfg(feature = "perf")]
            {
                if miss_reason == Ext4FstatMissReason::ColdInode {
                    let kind = match self.inode_type {
                        InodeType::File => Ext4FstatColdInodeKind::RegularFile,
                        InodeType::Dir => Ext4FstatColdInodeKind::Directory,
                        _ => Ext4FstatColdInodeKind::SpecialNode,
                    };
                    crate::utils::perf::record_ext4_fstat_cold_inode(kind, self.has_lookup_stat);
                }
                let _miss = Ext4FstatMissGuard::new(miss_reason);
                let mut fstat_stages = Ext4FstatStageRecorder::new();
                inner
                    .f
                    .fstat_with_perf_observer(|event| fstat_stages.record(event))
            }
            #[cfg(not(feature = "perf"))]
            {
                inner.f.fstat()
            }
        };
        let stat = match fstat_result {
            Ok(s) => s,
            Err(rc) => {
                {
                    #[cfg(feature = "perf")]
                    let _recovery = Ext4FstatRecoveryGuard::new();
                    let _ = self.recover_live_path(inner);
                }
                let recovery_fstat_result = {
                    #[cfg(feature = "perf")]
                    {
                        let _miss = Ext4FstatMissGuard::new(Ext4FstatMissReason::AliasRecovery);
                        let mut fstat_stages = Ext4FstatStageRecorder::new();
                        inner
                            .f
                            .fstat_with_perf_observer(|event| fstat_stages.record(event))
                    }
                    #[cfg(not(feature = "perf"))]
                    {
                        inner.f.fstat()
                    }
                };
                match recovery_fstat_result {
                    Ok(s) => s,
                    Err(_) => {
                        warn!(
                            "Ext4Inode::fstat: ext4_stat_get failed rc={}, path={:?}",
                            rc,
                            inner.f.path()
                        );
                        #[cfg(feature = "perf")]
                        fstat_path.finish(Ext4FstatPath::ActualExt4Fstat);
                        return Kstat::default();
                    }
                }
            }
        };
        let kstat = Self::kstat_from_ext4(stat);
        let cpath = inner.f.path();
        let path_str = cpath.to_str().unwrap_or("");
        let mut kstat = kstat;
        if let Some(node_type) = FsIndex::special_node_type(path_str) {
            let type_bits = node_type.mode_bits();
            kstat.st_mode = (kstat.st_mode & !0xF000) | type_bits;
        }
        self.update_cached_stat(kstat);
        self.update_cached_directory_stat(kstat);
        #[cfg(feature = "perf")]
        fstat_path.finish(Ext4FstatPath::ActualExt4Fstat);
        self.stat_with_known_size(kstat)
    }
    /// 读取目录项内容。
    ///
    /// `off` 是 lwext4 目录读取 cookie，不一定等价于普通字节偏移。
    pub(super) fn read_dentry_impl(&self, off: usize, len: usize) -> SysResult<(Vec<u8>, isize)> {
        // `read_dir_from` is the only lwext4 operation here.  Keep directory
        // entry serialization and mount-table inspection outside the global
        // guard so a large directory does not block unrelated file reads.
        let (path, entries) = {
            let _io_state = self.io_state.lock();
            let inner = self.inner.get_unchecked_mut();
            let path = Self::live_path(inner);
            let entries = inner.f.read_dir_from(off as u64).map_err(SysErrNo::from)?;
            (path, entries)
        };
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
        // Update dir atime unless MS_NOATIME or MS_NODIRATIME suppresses it.
        if res > 0 {
            let suppress = MNT_TABLE
                .lock()
                .mount_for_path(&path)
                .map(|(_, _, _, flags)| {
                    flags.intersects(MountFlags::NOATIME | MountFlags::NODIRATIME)
                })
                .unwrap_or(false);
            if !suppress {
                let now = crate::timer::realtime();
                let _ = self.set_timestamps_impl(Some(now.tv_sec as u64), None, None);
            }
        }
        // assert!(res != 0);
        Ok((de, f_off as isize))
    }

    /// 判断目录是否为空。
    ///
    /// 只要出现除 `.` 和 `..` 之外的目录项，就认为目录非空。
    pub(super) fn is_dir_empty_impl(&self) -> Result<bool, SysErrNo> {
        let _io_state = self.io_state.lock();
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

    /// 读取符号链接目标路径。
    pub(super) fn read_link_impl(&self, buf: &mut [u8], bufsize: usize) -> SysResult<usize> {
        let _io_state = self.io_state.lock();
        let inner = self.inner.get_unchecked_mut();
        let _ = Self::live_path(inner);
        let file = &mut inner.f;
        file.file_readlink(buf, bufsize).map_err(SysErrNo::from)
    }
    /// 获取硬链接计数。
    ///
    /// lwext4 在路径已不存在时可能返回 `ENOENT`，这里按 0 个 link 兼容延迟删除路径。
    pub(super) fn link_cnt_impl(&self) -> SyscallRet {
        let _io_state = self.io_state.lock();
        #[cfg(feature = "perf")]
        let _phase = Ext4InodePhaseGuard::metadata(Ext4MetadataPhase::LinkCount);
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
    /// 读取文件 mode bits。
    ///
    /// 当前路径失败时会尝试从 alias 恢复，兼容 rename/hard link 后的已打开 fd。
    pub(super) fn fmode_impl(&self) -> Result<u32, SysErrNo> {
        let _write_state = self.write_state.lock();
        let _io_state = self.io_state.lock();
        #[cfg(feature = "perf")]
        let _phase = Ext4InodePhaseGuard::metadata(Ext4MetadataPhase::Mode);
        let inner = self.inner.get_unchecked_mut();
        match inner.f.file_mode() {
            Ok(mode) => Ok(mode),
            Err(_) => {
                let _ = self.recover_live_path(inner);
                inner.f.file_mode().map_err(SysErrNo::from)
            }
        }
    }
    /// 设置文件 mode bits。
    ///
    /// 如果传入 mode 未带文件类型位，则沿用当前 inode 类型，避免 chmod 类操作把
    /// regular/dir/symlink 类型位清掉。
    pub(super) fn fmode_set_impl(&self, mode: u32) -> SyscallRet {
        let _write_state = self.write_state.lock();
        let _io_state = self.io_state.lock();
        #[cfg(feature = "perf")]
        let _phase = Ext4InodePhaseGuard::metadata(Ext4MetadataPhase::Mode);
        let inner = self.inner.get_unchecked_mut();
        let mode_type = mode & 0o170000;
        let mode_type = if mode_type != 0 {
            mode_type
        } else {
            as_inode_type(inner.f.file_type()).mode_bits()
        };
        let mode = mode_type | (mode & 0o7777);
        let ret = match inner.f.file_mode_set(mode) {
            Ok(ret) => Ok(ret),
            Err(_) => {
                let _ = self.recover_live_path(inner);
                inner.f.file_mode_set(mode).map_err(SysErrNo::from)
            }
        };
        if ret.is_ok() {
            if self.inode_type == InodeType::Dir {
                self.advance_local_directory_stat_epoch();
            }
            self.invalidate_cached_stat(Ext4FstatMissReason::Metadata);
        }
        ret
    }

    /// 设置 inode owner uid/gid。
    pub(super) fn owner_set_impl(&self, uid: u32, gid: u32) -> SyscallRet {
        // Keep owner updates in the filesystem layer so stat and permission checks agree.
        let _write_state = self.write_state.lock();
        let _io_state = self.io_state.lock();
        #[cfg(feature = "perf")]
        let _phase = Ext4InodePhaseGuard::metadata(Ext4MetadataPhase::Owner);
        let inner = self.inner.get_unchecked_mut();
        let ret = match inner.f.file_owner_set(uid, gid) {
            Ok(ret) => Ok(ret),
            Err(_) => {
                let _ = self.recover_live_path(inner);
                inner.f.file_owner_set(uid, gid).map_err(SysErrNo::from)
            }
        };
        if ret.is_ok() {
            if self.inode_type == InodeType::Dir {
                self.advance_local_directory_stat_epoch();
            }
            self.invalidate_cached_stat(Ext4FstatMissReason::Metadata);
        }
        ret
    }
}
