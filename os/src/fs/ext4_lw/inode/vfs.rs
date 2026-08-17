//! VFS trait boundary for the lwext4 inode adapter.
//!
//! Rust requires one coherent `Inode` implementation for `Ext4Inode`. Keep
//! that boundary thin here and place the operation bodies in the modules that
//! own their locking and cache semantics.

use super::*;

impl Inode for Ext4Inode {
    fn mark_directory_stat_changed(&self) {
        self.advance_local_directory_stat_epoch();
    }

    fn size(&self) -> usize {
        self.size_impl()
    }

    fn create(&self, path: &str, ty: InodeType) -> Result<Arc<dyn Inode>, SysErrNo> {
        self.create_impl(path, ty)
    }

    fn create_with_metadata(
        &self,
        path: &str,
        ty: InodeType,
        mode: u32,
        owner: Option<(u32, u32)>,
    ) -> Result<Arc<dyn Inode>, SysErrNo> {
        self.create_with_metadata_impl(path, ty, mode, owner)
    }

    fn create_dir_fast(&self, path: &str) -> Result<Arc<dyn Inode>, SysErrNo> {
        self.create_dir_fast_impl(path)
    }

    fn types(&self) -> InodeType {
        self.inode_type
    }

    fn read_at(&self, off: usize, buf: &mut [u8]) -> SyscallRet {
        self.read_at_impl(off, buf)
    }

    fn write_at(&self, off: usize, buf: &[u8]) -> SyscallRet {
        self.write_at_impl(off, buf)
    }

    fn truncate(&self, size: usize) -> SyscallRet {
        self.truncate_impl(size)
    }

    fn rename(&self, path: &str, new_path: &str) -> SyscallRet {
        self.rename_impl(path, new_path)
    }

    fn hard_link(&self, old_path: &str, new_path: &str) -> SyscallRet {
        self.hard_link_impl(old_path, new_path)
    }

    fn set_timestamps(
        &self,
        atime: Option<u64>,
        mtime: Option<u64>,
        ctime: Option<u64>,
    ) -> SyscallRet {
        self.set_timestamps_impl(atime, mtime, ctime)
    }

    fn set_xattr(&self, name: &[u8], value: &[u8], flags: u32) -> SyscallRet {
        self.set_xattr_impl(name, value, flags)
    }

    fn get_xattr(&self, name: &[u8], value: &mut [u8]) -> SyscallRet {
        self.get_xattr_impl(name, value)
    }

    fn list_xattr(&self, list: &mut [u8]) -> SyscallRet {
        self.list_xattr_impl(list)
    }

    fn remove_xattr(&self, name: &[u8]) -> SyscallRet {
        self.remove_xattr_impl(name)
    }

    fn sync(&self) {
        self.sync_impl()
    }

    fn read_all(&self) -> Result<Vec<u8>, SysErrNo> {
        self.read_all_impl()
    }

    fn find(
        &self,
        path: &str,
        flags: OpenFlags,
        loop_times: usize,
    ) -> Result<Arc<dyn Inode>, SysErrNo> {
        self.find_impl(path, flags, loop_times)
    }

    fn find_from_cached_parent(
        &self,
        path: &str,
        flags: OpenFlags,
    ) -> Result<Arc<dyn Inode>, SysErrNo> {
        self.find_from_cached_parent_impl(path, flags)
    }

    fn fstat(&self) -> Kstat {
        self.fstat_impl()
    }

    fn read_dentry(&self, off: usize, len: usize) -> SysResult<(Vec<u8>, isize)> {
        self.read_dentry_impl(off, len)
    }

    fn is_dir_empty(&self) -> Result<bool, SysErrNo> {
        self.is_dir_empty_impl()
    }

    fn read_link(&self, buf: &mut [u8], bufsize: usize) -> SysResult<usize> {
        self.read_link_impl(buf, bufsize)
    }

    fn sym_link(&self, target: &str, path: &str) -> SyscallRet {
        self.sym_link_impl(target, path)
    }

    fn link_cnt(&self) -> SyscallRet {
        self.link_cnt_impl()
    }

    fn unlink(&self, path: &str) -> SyscallRet {
        self.unlink_impl(path)
    }

    fn path(&self) -> String {
        self.cached_path()
    }

    fn page_cache_path(&self) -> Option<Arc<str>> {
        Some(self.cached_page_cache_path())
    }

    fn cache_identity(&self) -> Option<(usize, usize)> {
        self.inode_identity
    }

    fn cache_identity_is_current(&self) -> bool {
        self.inode_identity.is_some()
            && self.identity_epoch == EXT4_IDENTITY_EPOCH.load(Ordering::Acquire)
    }

    #[cfg(feature = "perf")]
    fn mark_fstat_cache_fsidx_rebuild(&self) {
        self.mark_stat_cache_fsidx_rebuild();
    }

    fn cache_path_alias(&self, path: &str) {
        self.add_alias_path(path);
    }

    fn remap_path_prefix(&self, old_prefix: &str, new_prefix: &str) {
        self.remap_path_prefix_impl(old_prefix, new_prefix);
    }

    fn delay(&self) {
        self.delay_impl();
    }

    fn fmode(&self) -> Result<u32, SysErrNo> {
        self.fmode_impl()
    }

    fn fmode_set(&self, mode: u32) -> SyscallRet {
        self.fmode_set_impl(mode)
    }

    fn owner_set(&self, uid: u32, gid: u32) -> SyscallRet {
        self.owner_set_impl(uid, gid)
    }

    fn seek_data(&self, offset: usize) -> SyscallRet {
        self.seek_data_impl(offset)
    }

    fn seek_hole(&self, offset: usize) -> SyscallRet {
        self.seek_hole_impl(offset)
    }
}
