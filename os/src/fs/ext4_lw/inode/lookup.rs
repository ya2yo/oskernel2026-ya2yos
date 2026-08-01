//! Path lookup and symbolic-link resolution for the lwext4 inode adapter.

use super::*;

impl Ext4Inode {
    /// 在路径中查找节点，支持递归解析符号链接。
    ///
    /// lwext4 没有暴露当前父目录句柄下的相对子项查找接口，因此这里仍使用 path-based
    /// `check_inode_exist()`。`O_NOFOLLOW`、`O_DIRECTORY` 和内部 `O_UNLINK` 会影响
    /// symlink/目录的返回语义。
    pub(super) fn find_impl(
        &self,
        path: &str,
        flags: OpenFlags,
        loop_times: usize,
    ) -> Result<Arc<dyn Inode>, SysErrNo> {
        // log::info!("[Inode.find] origin path={}", path);
        let skip_intermediate_retry = loop_times == SKIP_INTERMEDIATE_SYMLINK_RETRY;
        let symlink_depth = if skip_intermediate_retry {
            0
        } else {
            loop_times
        };
        let result = {
            let _io_state = self.io_state.lock();
            let _ext4 = EXT4_OP_LOCK.lock_for_find();
            let file = &mut self.inner.get_unchecked_mut().f;
            // Capture the epoch while the same global gate protects the
            // lookup result.  It must travel with `stat`, not be sampled
            // later while constructing the VFS wrapper outside this guard.
            let lookup_identity_epoch = EXT4_IDENTITY_EPOCH.load(Ordering::Acquire);
            let lookup_directory_stat_epoch = EXT4_DIRECTORY_STAT_EPOCH.load(Ordering::Acquire);
            match file.inode_type_and_stat_at(path) {
                Ok((InodeTypes::EXT4_DE_DIR | InodeTypes::EXT4_INODE_MODE_DIRECTORY, stat)) => {
                    Ext4FindResult::Dir {
                        stat,
                        identity_epoch: lookup_identity_epoch,
                        directory_stat_epoch: lookup_directory_stat_epoch,
                    }
                }
                Ok((InodeTypes::EXT4_DE_REG_FILE | InodeTypes::EXT4_INODE_MODE_FILE, stat)) => {
                    Ext4FindResult::File {
                        stat,
                        identity_epoch: lookup_identity_epoch,
                    }
                }
                Ok((InodeTypes::EXT4_DE_SYMLINK | InodeTypes::EXT4_INODE_MODE_SOFTLINK, stat)) => {
                    Ext4FindResult::SymLink(stat)
                }
                // Keep the existing behavior for unsupported special nodes
                // and lookup errors: callers can still retry an intermediate
                // symlink before receiving ENOENT.
                _ => Ext4FindResult::Missing,
            }
        };

        let is_symlink = match result {
            Ext4FindResult::Dir {
                stat,
                identity_epoch,
                directory_stat_epoch,
            } => {
                return Ok(Arc::new(Ext4Inode::new_with_stat(
                    path,
                    InodeTypes::EXT4_DE_DIR,
                    stat,
                    identity_epoch,
                    Some(directory_stat_epoch),
                )));
            }
            Ext4FindResult::File {
                stat,
                identity_epoch,
            } => {
                if flags.contains(OpenFlags::O_DIRECTORY) {
                    return Err(SysErrNo::ENOTDIR);
                }
                return Ok(Arc::new(Ext4Inode::new_with_stat(
                    path,
                    InodeTypes::EXT4_DE_REG_FILE,
                    stat,
                    identity_epoch,
                    None,
                )));
            }
            Ext4FindResult::SymLink(_) => {
                if flags.contains(OpenFlags::O_NOFOLLOW) {
                    return Err(SysErrNo::ELOOP);
                }
                true
            }
            Ext4FindResult::Missing => false,
        };

        if !is_symlink {
            if !skip_intermediate_retry && symlink_depth < MAX_LOOPTIMES {
                if let Some(next_path) = self.resolve_intermediate_symlink(path)? {
                    return self.find_impl(&next_path, flags, symlink_depth + 1);
                }
            }
            return Err(SysErrNo::ENOENT);
        }
        if flags.contains(OpenFlags::O_UNLINK) {
            return Ok(Arc::new(Ext4Inode::new(path, InodeTypes::EXT4_DE_SYMLINK)));
        }
        if flags.contains(OpenFlags::O_NOFOLLOW) || symlink_depth >= MAX_LOOPTIMES {
            return Err(SysErrNo::ELOOP);
        }

        let mut file_name = [0u8; 256];
        let file = Ext4Inode::new(path, InodeTypes::EXT4_DE_SYMLINK);
        file.read_link(&mut file_name, 256)?;
        let end = file_name
            .iter()
            .position(|v| *v == 0)
            .unwrap_or(file_name.len());
        let file_path = core::str::from_utf8(&file_name[..end]).unwrap();
        let next_path = if file_path.starts_with('/') {
            file_path.to_string()
        } else {
            join_path(path, file_path)
        };
        self.find_impl(&next_path, flags, symlink_depth + 1)
    }

    /// A cached parent inode denotes an already-resolved directory.  If its
    /// direct child is absent, there cannot be an unresolved intermediate
    /// symlink in this final lookup path, so avoid the fallback prefix scan.
    /// A final symlink still follows the ordinary recursive path above.
    pub(super) fn find_from_cached_parent_impl(
        &self,
        path: &str,
        flags: OpenFlags,
    ) -> Result<Arc<dyn Inode>, SysErrNo> {
        self.find_impl(path, flags, SKIP_INTERMEDIATE_SYMLINK_RETRY)
    }
}
