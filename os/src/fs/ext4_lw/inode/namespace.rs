//! Namespace-changing operations for the lwext4 inode adapter.

use super::*;

impl Ext4Inode {
    /// 在当前文件系统中创建一个新 inode。
    ///
    /// `path` 必须是绝对路径。目录通过 `dir_mk()` 创建，普通文件通过
    /// `file_open(O_CREAT|O_TRUNC)` 创建后立即关闭。目标已存在时返回 `EEXIST`，
    /// 用于承载 Linux `O_CREAT|O_EXCL` 语义。
    ///
    /// 新文件的描述符关闭不立即刷新全局 block cache；创建路径通常还会设置
    /// mode/owner 或写入内容，由上层在完整操作结束后统一同步。
    pub(super) fn create_impl(
        &self,
        path: &str,
        ty: InodeType,
    ) -> Result<Arc<dyn Inode>, SysErrNo> {
        let types = as_ext4_de_type(ty);
        // Construct before taking the parent VFS state so error paths can
        // close the descriptor without lock-order inversion.
        let nf = Ext4Inode::new(path, types.clone());
        let _io_state = self.io_state.lock();
        #[cfg(feature = "perf")]
        let _phase = Ext4InodePhaseGuard::namespace(Ext4NamespacePhase::Create);

        let nfile = &mut nf.inner.get_unchecked_mut().f;
        if types == InodeTypes::EXT4_DE_DIR {
            #[cfg(feature = "perf")]
            let _create_phase = Ext4CreatePhaseGuard::new(Ext4CreatePhase::DirMkOrFileOpen);
            if let Err(e) = nfile.dir_mk_exclusive(path) {
                return Err(SysErrNo::from(e));
            }
        } else {
            {
                #[cfg(feature = "perf")]
                let _create_phase = Ext4CreatePhaseGuard::new(Ext4CreatePhase::DirMkOrFileOpen);
                if let Err(e) = nfile.file_open(path, O_RDWR | O_CREAT | O_EXCL | O_TRUNC) {
                    return Err(SysErrNo::from(e));
                }
            }
            #[cfg(feature = "perf")]
            let _create_phase = Ext4CreatePhaseGuard::new(Ext4CreatePhase::FileClose);
            nfile.file_close_without_cache_flush()?;
        }
        // Both file and directory creation add one entry to this inode's
        // directory, changing only this parent directory's mtime/ctime.
        {
            #[cfg(feature = "perf")]
            let _create_phase = Ext4CreatePhaseGuard::new(Ext4CreatePhase::VfsFinish);
            self.advance_local_directory_stat_epoch();
        }
        Ok(Arc::new(nf))
    }

    /// Create a node and apply its initial metadata while retaining the
    /// namespace operation lock.  The creation transaction also returns the
    /// new inode's stat, so publishing it to `FsIndex` does not require an
    /// immediate path-based fstat.
    pub(super) fn create_with_metadata_impl(
        &self,
        path: &str,
        ty: InodeType,
        mode: u32,
        owner: Option<(u32, u32)>,
    ) -> Result<Arc<dyn Inode>, SysErrNo> {
        let types = as_ext4_de_type(ty);
        // Construct before taking the parent/global locks so error paths drop
        // the descriptor only after those guards have been released.
        let mut nfile = Ext4File::new(path, types.clone());

        // The requested mode normally contains permission bits only.  The
        // VFS already knows the inode type, so derive the type bits without
        // reopening the just-created path through ext4_mode_get().
        let mode_type = mode & 0o170000;
        let mode = if mode_type != 0 {
            mode
        } else {
            as_inode_type(types.clone()).mode_bits() | (mode & 0o7777)
        };
        let (uid, gid) = owner.unwrap_or((0, 0));

        let is_directory = types == InodeTypes::EXT4_DE_DIR;
        let (stat, identity_epoch, directory_stat_epoch) = {
            let _io_state = self.io_state.lock();
            #[cfg(feature = "perf")]
            let _phase = Ext4InodePhaseGuard::namespace(Ext4NamespacePhase::Create);
            let stat = if is_directory {
                #[cfg(feature = "perf")]
                let _create_phase = Ext4CreatePhaseGuard::new(Ext4CreatePhase::DirMkOrFileOpen);
                nfile
                    .dir_mk_exclusive_with_metadata(path, mode, uid, gid)
                    .map_err(SysErrNo::from)?
            } else {
                let stat = {
                    #[cfg(feature = "perf")]
                    let _create_phase = Ext4CreatePhaseGuard::new(Ext4CreatePhase::DirMkOrFileOpen);
                    nfile
                        .file_open_with_metadata(
                            path,
                            O_RDWR | O_CREAT | O_EXCL | O_TRUNC,
                            mode,
                            uid,
                            gid,
                        )
                        .map_err(SysErrNo::from)?
                };
                {
                    #[cfg(feature = "perf")]
                    let _create_phase = Ext4CreatePhaseGuard::new(Ext4CreatePhase::FileClose);
                    nfile.file_close_without_cache_flush()?;
                }
                stat
            };
            {
                #[cfg(feature = "perf")]
                let _create_phase = Ext4CreatePhaseGuard::new(Ext4CreatePhase::VfsFinish);
                self.advance_local_directory_stat_epoch();
            }
            (
                stat,
                EXT4_IDENTITY_EPOCH.load(Ordering::Acquire),
                EXT4_DIRECTORY_STAT_EPOCH.load(Ordering::Acquire),
            )
        };

        Ok(Arc::new(Ext4Inode::new_with_stat(
            path,
            types,
            stat,
            identity_epoch,
            is_directory.then_some(directory_stat_epoch),
        )))
    }

    /// 创建内核维护的目录项。
    ///
    /// `ext4_dir_mk()` 自身已经完成“存在则打开，不存在则创建”的路径处理；
    /// proc 目录由 PID 分配器保证名称唯一，因此无需再由 VFS 先执行一次
    /// `check_inode_exist()`。
    pub(super) fn create_dir_fast_impl(&self, path: &str) -> Result<Arc<dyn Inode>, SysErrNo> {
        let nf = Ext4Inode::new(path, InodeTypes::EXT4_DE_DIR);
        #[cfg(feature = "perf")]
        let _phase = Ext4InodePhaseGuard::namespace(Ext4NamespacePhase::Create);
        let nfile = &mut nf.inner.get_unchecked_mut().f;
        {
            #[cfg(feature = "perf")]
            let _create_phase = Ext4CreatePhaseGuard::new(Ext4CreatePhase::DirMkOrFileOpen);
            nfile.dir_mk(path).map_err(SysErrNo::from)?;
        }
        {
            #[cfg(feature = "perf")]
            let _create_phase = Ext4CreatePhaseGuard::new(Ext4CreatePhase::VfsFinish);
            self.advance_local_directory_stat_epoch();
        }
        Ok(Arc::new(nf))
    }
    /// 重命名当前 inode 对应的路径。
    ///
    /// 成功后把新路径加入 alias，并将内部 `Ext4File` 切换到新路径，减少后续元数据操作
    /// 依赖 fallback 恢复路径的次数。
    pub(super) fn rename_impl(&self, path: &str, new_path: &str) -> SyscallRet {
        if path == new_path {
            return Ok(0);
        }

        // Directory rename changes the source and possibly replaced target
        // metadata.  Retain the target handle so a successful replacement can
        // invalidate only that directory instead of the whole mount.
        let cached_target = if self.inode_type == InodeType::Dir {
            FsIndex::find_inode_idx(new_path)
        } else {
            None
        };
        let _write_state = self.write_state.lock();
        let _io_state = self.io_state.lock();
        #[cfg(feature = "perf")]
        let write_back_phase = Ext4InodePhaseGuard::rename(Ext4RenamePhase::WriteBackCache);
        let inner = self.inner.get_unchecked_mut();
        let types = inner.f.types();
        let active_path = inner.f.path().into_string().unwrap();

        // Sparse ranges must be published before the directory entry moves.
        // Dense byte caches are migrated after a successful rename so their
        // dirty contents remain visible without a full serialized write-back.
        inner
            .f
            .flush_sparse_write_buffer_for_rename()
            .map_err(SysErrNo::from)?;
        #[cfg(feature = "perf")]
        drop(write_back_phase);
        // Closing with the normal helper would call ext4_cache_flush() and
        // serialize every dirty block on this mount.
        {
            #[cfg(feature = "perf")]
            let _phase = Ext4InodePhaseGuard::rename(Ext4RenamePhase::Close);
            inner
                .f
                .file_close_without_cache_flush()
                .map_err(SysErrNo::from)?;
        }
        let migrated_cache = {
            #[cfg(feature = "perf")]
            let _phase = Ext4InodePhaseGuard::rename(Ext4RenamePhase::Lwext4Rename);
            inner
                .f
                .file_rename(path, new_path)
                .map_err(SysErrNo::from)?;
            rename_path_cache(path, new_path).is_some()
        };
        // `rename()` can replace an existing destination inode.  Invalidate
        // identity-epoch proofs before the rename result is published so a
        // later FsIndex collision retains the live `fstat()` reuse check.
        Self::advance_identity_epoch();
        if self.inode_type == InodeType::Dir {
            // Keep directory metadata invalidation local.  If the destination
            // was not cached, retain the old conservative global fallback for
            // a potentially live-but-unindexed replacement inode.
            self.advance_local_directory_stat_epoch();
            Self::advance_directory_stat_epoch_for_path(path);
            if Self::parent_path(path) != Self::parent_path(new_path) {
                Self::advance_directory_stat_epoch_for_path(new_path);
            }
            if let Some(target) = cached_target.as_ref() {
                Self::mark_cached_directory_stat_inode(target);
            } else {
                Self::advance_global_directory_stat_epoch();
            }
        } else {
            Self::advance_directory_stat_epoch_for_path(path);
            if Self::parent_path(path) != Self::parent_path(new_path) {
                Self::advance_directory_stat_epoch_for_path(new_path);
            }
        }

        // A successful directory-entry move must not leave an orphaned
        // write-back entry for either pathname.  In particular, stale target
        // state could otherwise overwrite Rustc's newly published artifact.
        {
            #[cfg(feature = "perf")]
            let _phase = Ext4InodePhaseGuard::rename(Ext4RenamePhase::VfsCacheInvalidate);
            if !migrated_cache {
                discard_path_cache(path);
                discard_path_cache(new_path);
            } else {
                discard_path_cache(path);
            }

            inner.aliases.retain(|alias| alias.as_str() != path);
            if inner.aliases.iter().all(|alias| alias != new_path) {
                inner.aliases.push(new_path.to_string());
            }
            inner.f = Ext4File::new(new_path, types);
            self.update_cached_path(new_path);
            self.invalidate_cached_stat(Ext4FstatMissReason::Rename);
            FILE_PAGE_CACHE.invalidate_path(&active_path);
            FILE_PAGE_CACHE.invalidate_path(path);
            FILE_PAGE_CACHE.invalidate_path(new_path);
        }
        Ok(0)
    }

    /// 创建硬链接：`new_path` 指向 `old_path` 相同的 inode。
    ///
    /// 成功后把新路径加入 alias，以便原路径 unlink 后已打开 fd 仍有可用路径。
    pub(super) fn hard_link_impl(&self, old_path: &str, new_path: &str) -> SyscallRet {
        let _write_state = self.write_state.lock();
        let _io_state = self.io_state.lock();
        #[cfg(feature = "perf")]
        let _phase = Ext4InodePhaseGuard::namespace(Ext4NamespacePhase::LinkSymlink);
        let inner = self.inner.get_unchecked_mut();
        let file = &mut inner.f;
        let ret = file
            .file_hardlink(old_path, new_path)
            .map_or(Err(SysErrNo::ENOENT), |_| Ok(0));
        if ret.is_ok() {
            if inner.aliases.iter().all(|alias| alias != new_path) {
                inner.aliases.push(new_path.to_string());
            }
            Self::advance_directory_stat_epoch_for_path(new_path);
            self.invalidate_cached_stat(Ext4FstatMissReason::HardLink);
        }
        ret
    }
    /// 创建符号链接。
    pub(super) fn sym_link_impl(&self, target: &str, path: &str) -> SyscallRet {
        let _io_state = self.io_state.lock();
        #[cfg(feature = "perf")]
        let _phase = Ext4InodePhaseGuard::namespace(Ext4NamespacePhase::LinkSymlink);
        let file = &mut self.inner.get_unchecked_mut().f;
        let ret = file.file_fsymlink(target, path).map_err(SysErrNo::from);
        if ret.is_ok() {
            self.advance_local_directory_stat_epoch();
        }
        ret
    }
    /// 删除指定路径的目录项。
    ///
    /// 目录走 `dir_rm()`，普通文件和其他文件类型走 `file_remove()`。
    pub(super) fn unlink_impl(&self, path: &str) -> SyscallRet {
        let _write_state = self.write_state.lock();
        let _io_state = self.io_state.lock();
        let (ret, remove_quota) = {
            #[cfg(feature = "perf")]
            let _phase = Ext4InodePhaseGuard::namespace(Ext4NamespacePhase::Unlink);
            let inner = self.inner.get_unchecked_mut();
            let is_dir = as_inode_type(inner.f.types()) == InodeType::Dir;
            let file = &mut inner.f;
            let ret = if is_dir {
                file.dir_rm(path).map_err(SysErrNo::from)
            } else {
                file.file_remove(path).map_err(SysErrNo::from).map(|_| 0)
            };
            if ret.is_ok() {
                // A removed directory entry can eventually make this inode number
                // available to another path.  Existing wrappers from before this
                // point therefore require FsIndex's live identity validation.
                Self::advance_identity_epoch();
                if is_dir {
                    // The removed inode and its cached parent are known; only
                    // an uncached parent needs the mount-wide fallback.
                    self.advance_local_directory_stat_epoch();
                    Self::advance_directory_stat_epoch_for_path(path);
                } else {
                    Self::advance_directory_stat_epoch_for_path(path);
                }
                self.invalidate_cached_stat(Ext4FstatMissReason::Unlink);
            }
            let remove_quota = !is_dir && ret.is_ok();
            (ret, remove_quota)
        };
        if remove_quota {
            MNT_TABLE.lock().remove_file(path);
        }
        ret
    }
    /// 标记为延迟删除。
    ///
    /// 当文件已经 unlink 但仍有 fd 持有 inode 时，先标记延迟删除，等最后一个
    /// `Arc<Ext4Inode>` drop 时再真正移除磁盘文件。
    pub(super) fn delay_impl(&self) {
        let _write_state = self.write_state.lock();
        let _io_state = self.io_state.lock();
        #[cfg(feature = "perf")]
        let _phase = Ext4InodePhaseGuard::metadata(Ext4MetadataPhase::Delay);
        self.inner.get_unchecked_mut().delay = true;
        self.delayed.store(true, Ordering::Release);
    }
}
