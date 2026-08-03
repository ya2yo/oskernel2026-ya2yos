use crate::bindings::*;
use alloc::boxed::Box;
use alloc::ffi::CString;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::UnsafeCell;
use core::convert::TryFrom;
use core::ffi::{c_char, c_void};
use core::ptr::null_mut;
use core::slice::{from_raw_parts, from_raw_parts_mut};

/// Device block size.
const EXT4_DEV_BSIZE: u32 = 512;

/// Wait callback used by the C block cache while another hart owns a buffer
/// state transition.
pub type BcacheWaitFn = unsafe extern "C" fn(
    ctx: *mut c_void,
    flags: *const core::ffi::c_int,
    mask: core::ffi::c_int,
    lba: u64,
) -> core::ffi::c_int;

/// Wake callback paired with [`BcacheWaitFn`].
pub type BcacheWakeFn = unsafe extern "C" fn(ctx: *mut c_void, lba: u64);

/// Callback installed by a preemptible kernel for one lwext4 resource lock.
/// `lock` is the stable address of an `ext4_fs` member and `write` selects
/// exclusive versus shared acquisition.
pub type FsRwlockLockHook = unsafe extern "C" fn(ctx: *mut c_void, lock: *mut c_void, write: bool);

/// Callback paired with [`FsRwlockLockHook`].
pub type FsRwlockUnlockHook =
    unsafe extern "C" fn(ctx: *mut c_void, lock: *mut c_void, write: bool);

/// Reports whether the current task owns a resource lock's write side.
pub type FsRwlockWriteOwnedHook =
    unsafe extern "C" fn(ctx: *mut c_void, lock: *const c_void) -> bool;

unsafe extern "C" {
    #[link_name = "ext4_bcache_setup_sync"]
    fn ext4_bcache_setup_sync_ffi(
        ctx: *mut c_void,
        wait: Option<BcacheWaitFn>,
        wake: Option<BcacheWakeFn>,
    );

    #[link_name = "ext4_fs_rwlock_set_hooks"]
    fn ext4_fs_rwlock_set_hooks_ffi(
        ctx: *mut c_void,
        lock_hook: Option<FsRwlockLockHook>,
        unlock_hook: Option<FsRwlockUnlockHook>,
        write_owned_hook: Option<FsRwlockWriteOwnedHook>,
    );
}

pub trait KernelDevOp {
    /// The device object retained by the lwext4 block-device callback.
    ///
    /// Callbacks may be invoked from different harts after the lwext4 SMP
    /// work is enabled. Implementors must therefore make these shared
    /// operations safe; they must not depend on a mutable seek cursor.
    type DevType;

    /// Return the total addressable size in bytes.
    fn device_size(dev: &Self::DevType) -> Result<u64, i32>;

    /// Read one complete request starting at `offset`.
    ///
    /// A successful result must equal `buf.len()`. Returning a short count is
    /// treated as an I/O error by the lwext4 adapter.
    fn read_at(dev: &Self::DevType, offset: u64, buf: &mut [u8]) -> Result<usize, i32>;

    /// Write one complete request starting at `offset`.
    ///
    /// A successful result must equal `buf.len()`. The implementation owns
    /// any serialization required for an unaligned read-modify-write.
    fn write_at(dev: &Self::DevType, offset: u64, buf: &[u8]) -> Result<usize, i32>;

    /// Flush device-visible writes after lwext4 has flushed its block cache.
    fn flush(dev: &Self::DevType) -> Result<usize, i32>;
}

pub struct Ext4BlockWrapper<K: KernelDevOp> {
    /// lwext4 mutates this object through its C ABI while the resource locks
    /// installed by the host serialize the relevant fields.  Keeping the
    /// foreign-owned state in an `UnsafeCell` makes the interior mutability
    /// explicit instead of manufacturing aliased `&mut` references from the
    /// VFS superblock.
    value: Box<UnsafeCell<ext4_blockdev>>,
    //block_dev: K::DevType,
    name: [u8; 16],
    mount_point: [u8; 32],
    pd: core::marker::PhantomData<K>,
}

impl<K: KernelDevOp> Ext4BlockWrapper<K> {
    /// Install the host kernel's task-aware implementation for all lwext4
    /// resource locks.  Call this before constructing the first wrapper so
    /// mount and recovery use the same waiting discipline as normal I/O.
    ///
    /// The C library intentionally keeps a raw atomic fallback when these
    /// hooks are absent, which preserves its standalone test environment.
    pub fn setup_fs_rwlock_hooks(
        ctx: *mut c_void,
        lock_hook: Option<FsRwlockLockHook>,
        unlock_hook: Option<FsRwlockUnlockHook>,
        write_owned_hook: Option<FsRwlockWriteOwnedHook>,
    ) {
        unsafe { ext4_fs_rwlock_set_hooks_ffi(ctx, lock_hook, unlock_hook, write_owned_hook) }
    }

    pub fn new(block_dev: K::DevType) -> Result<Self, i32> {
        // note this ownership
        let devt_user = Box::into_raw(Box::new(block_dev)) as *mut c_void;
        //let devt_user = devt.as_mut() as *mut _ as *mut c_void;
        //let devt_user = &mut block_dev as *mut _ as *mut c_void;

        // Block size buffer
        let bbuf = Box::new([0u8; EXT4_DEV_BSIZE as usize]);

        let ext4bdif: ext4_blockdev_iface = ext4_blockdev_iface {
            open: Some(Self::dev_open),
            bread: Some(Self::dev_bread),
            bwrite: Some(Self::dev_bwrite),
            close: Some(Self::dev_close),
            lock: None,
            unlock: None,
            ph_bsize: EXT4_DEV_BSIZE,
            ph_bcnt: 0,
            ph_bbuf: Box::into_raw(bbuf) as *mut u8,
            ph_refctr: 0,
            bread_ctr: 0,
            bwrite_ctr: 0,
            p_user: devt_user,
        };

        let bcbuf: Box<ext4_bcache> = Box::new(unsafe { core::mem::zeroed() });

        let ext4dev = ext4_blockdev {
            bdif: Box::into_raw(Box::new(ext4bdif)),
            part_offset: 0,
            part_size: 0 * EXT4_DEV_BSIZE as u64,
            bc: Box::into_raw(bcbuf),
            lg_bsize: 0,
            lg_bcnt: 0,
            cache_write_back: 0,
            fs: null_mut(),
            journal: null_mut(),
        };

        let c_name = CString::new("ext4_fs").expect("CString::new ext4_fs failed");
        let c_name = c_name.as_bytes_with_nul(); // + '\0'
                                                 //let c_mountpoint = CString::new("/mp/").unwrap();
        let c_mountpoint = CString::new("/").unwrap();
        let c_mountpoint = c_mountpoint.as_bytes_with_nul();

        let mut name: [u8; 16] = [0; 16];
        let mut mount_point: [u8; 32] = [0; 32];
        name[..c_name.len()].copy_from_slice(c_name);
        mount_point[..c_mountpoint.len()].copy_from_slice(c_mountpoint);

        let mut ext4bd = Self {
            value: Box::new(UnsafeCell::new(ext4dev)),
            //block_dev,
            name,
            mount_point,
            pd: core::marker::PhantomData,
        };

        info!("New an Ext4 Block Device");
        ext4bd.ext4_set_debug();

        // ext4_blockdev into static instance
        // lwext4_mount
        // let c_mountpoint = c_mountpoint as *const _ as *const c_char;
        unsafe {
            ext4bd
                .lwext4_mount()
                .expect("Failed to mount the ext4 file system, perhaps the disk is not an EXT4 file system.");
        }

        // ext4bd.lwext4_dir_ls();
        ext4bd.print_lwext4_mp_stats();
        ext4bd.print_lwext4_block_stats();

        Ok(ext4bd)
    }

    /// Install task-runtime wait/wake hooks for C block-cache single-flight
    /// loads. The callbacks must remain valid until the filesystem is
    /// unmounted; `ctx` is passed through unchanged.
    pub fn setup_bcache_sync(
        &self,
        ctx: *mut c_void,
        wait: Option<BcacheWaitFn>,
        wake: Option<BcacheWakeFn>,
    ) {
        unsafe { ext4_bcache_setup_sync_ffi(ctx, wait, wake) }
    }

    /// Recover the shared device object retained in `ext4_blockdev_iface`.
    ///
    /// This deliberately creates a shared reference. The former adapter made
    /// a fresh `&mut` for every C callback, which becomes undefined behaviour
    /// as soon as two harts issue requests concurrently.
    unsafe fn device_from_bdev<'a>(bdev: *mut ext4_blockdev) -> Option<&'a K::DevType> {
        let bdev = bdev.as_ref()?;
        let bdif = bdev.bdif.as_ref()?;
        (bdif.p_user as *const K::DevType).as_ref()
    }

    /// Convert an lwext4 physical-block request into a byte-addressed device
    /// request without truncation. The kernel implementation checks the final
    /// range against the actual device capacity.
    unsafe fn request_range(
        bdev: *mut ext4_blockdev,
        blk_id: u64,
        blk_cnt: u32,
    ) -> Option<(u64, usize)> {
        let bdev = bdev.as_ref()?;
        let bdif = bdev.bdif.as_ref()?;
        let block_size = u64::from(bdif.ph_bsize);
        let offset = blk_id.checked_mul(block_size)?;
        let len = block_size.checked_mul(u64::from(blk_cnt))?;
        Some((offset, usize::try_from(len).ok()?))
    }

    pub unsafe extern "C" fn dev_open(bdev: *mut ext4_blockdev) -> ::core::ffi::c_int {
        let Some(devt) = (unsafe { Self::device_from_bdev(bdev) }) else {
            error!("ext4 block device has no backing device");
            return EIO as _;
        };
        let size = match K::device_size(devt) {
            Ok(size) => size,
            Err(e) => {
                error!("dev_open to K::device_size failed: {:?}", e);
                return EFAULT as _;
            }
        };
        let Some(bdev_ref) = (unsafe { bdev.as_mut() }) else {
            return EIO as _;
        };
        let Some(bdif) = (unsafe { bdev_ref.bdif.as_mut() }) else {
            return EIO as _;
        };

        bdev_ref.part_offset = 0;
        bdev_ref.part_size = size;
        bdif.ph_bcnt = size / u64::from(bdif.ph_bsize);
        EOK as _
    }
    pub unsafe extern "C" fn dev_bread(
        bdev: *mut ext4_blockdev,
        buf: *mut ::core::ffi::c_void,
        blk_id: u64,
        blk_cnt: u32,
    ) -> ::core::ffi::c_int {
        if blk_cnt == 0 {
            return EOK as _;
        }
        if buf.is_null() {
            return EIO as _;
        }
        let Some(devt) = (unsafe { Self::device_from_bdev(bdev) }) else {
            return EIO as _;
        };
        let Some((offset, buf_len)) = (unsafe { Self::request_range(bdev, blk_id, blk_cnt) })
        else {
            return EIO as _;
        };
        let buffer = unsafe { from_raw_parts_mut(buf as *mut u8, buf_len) };

        match K::read_at(devt, offset, buffer) {
            Ok(read) if read == buf_len => EOK as _,
            Ok(read) => {
                error!(
                    "short ext4 block read: offset={:#x}, expected={}, actual={}",
                    offset, buf_len, read
                );
                EIO as _
            }
            Err(e) => {
                error!("ext4 block read failed at {:#x}: {:?}", offset, e);
                EIO as _
            }
        }
    }
    pub unsafe extern "C" fn dev_bwrite(
        bdev: *mut ext4_blockdev,
        buf: *const ::core::ffi::c_void,
        blk_id: u64,
        blk_cnt: u32,
    ) -> ::core::ffi::c_int {
        if blk_cnt == 0 {
            return EOK as _;
        }
        if buf.is_null() {
            return EIO as _;
        }
        let Some(devt) = (unsafe { Self::device_from_bdev(bdev) }) else {
            return EIO as _;
        };
        let Some((offset, buf_len)) = (unsafe { Self::request_range(bdev, blk_id, blk_cnt) })
        else {
            return EIO as _;
        };
        let buffer = unsafe { from_raw_parts(buf as *const u8, buf_len) };

        match K::write_at(devt, offset, buffer) {
            Ok(written) if written == buf_len => EOK as _,
            Ok(written) => {
                error!(
                    "short ext4 block write: offset={:#x}, expected={}, actual={}",
                    offset, buf_len, written
                );
                EIO as _
            }
            Err(e) => {
                error!("ext4 block write failed at {:#x}: {:?}", offset, e);
                EIO as _
            }
        }
    }
    pub unsafe extern "C" fn dev_close(_bdev: *mut ext4_blockdev) -> ::core::ffi::c_int {
        debug!("CLOSE Ext4 block device");
        //fclose(dev_file);
        EOK as _
    }

    pub fn sync(&self) -> Result<usize, i32> {
        unsafe {
            let bdev = self.value.get();
            let r = ext4_block_cache_flush(bdev);
            if r != EOK as i32 {
                error!("ext4_block_cache_flush: rc = {:?}\n", r);
                return Err(r);
            }
            let dev = Self::device_from_bdev(bdev).ok_or(EIO as i32)?;
            K::flush(dev)
        }
    }

    pub unsafe fn lwext4_mount(&mut self) -> Result<usize, i32> {
        let c_name = &self.name as *const _ as *const c_char;
        let c_mountpoint = &self.mount_point as *const _ as *const c_char;

        let r = ext4_device_register(self.value.get(), c_name);
        if r != EOK as i32 {
            error!("ext4_device_register: rc = {:?}\n", r);
            return Err(r);
        }
        let r = ext4_mount(c_name, c_mountpoint, false);
        if r != EOK as i32 {
            error!("ext4_mount: rc = {:?}\n", r);
            return Err(r);
        }
        let r = ext4_recover(c_mountpoint);
        if (r != EOK as i32) && (r != ENOTSUP as i32) {
            error!("ext4_recover: rc = {:?}\n", r);
            return Err(r);
        }

        //  ext4_mount("sda1", "/");
        //  ext4_journal_start("/");
        //
        // File operations here...
        //
        //  ext4_journal_stop("/");
        //  ext4_umount("/");
        let r = ext4_journal_start(c_mountpoint);
        if r != EOK as i32 {
            error!("ext4_journal_start: rc = {:?}\n", r);
            return Err(r);
        }
        ext4_cache_write_back(c_mountpoint, true);
        // ext4_bcache

        debug!("lwext4 mount Okay");
        Ok(0)
    }

    /// Call this when block device is being uninstalled
    pub fn lwext4_umount(&mut self) -> Result<usize, i32> {
        let c_name = &self.name as *const _ as *const c_char;
        let c_mountpoint = &self.mount_point as *const _ as *const c_char;

        unsafe {
            ext4_cache_write_back(c_mountpoint, false);

            let r = ext4_journal_stop(c_mountpoint);
            if r != EOK as i32 {
                error!("ext4_journal_stop: fail {}", r);
                return Err(r);
            }

            let r = ext4_umount(c_mountpoint);
            if r != EOK as i32 {
                error!("ext4_umount: fail {}", r);
                return Err(r);
            }

            let r = ext4_device_unregister(c_name);
            if r != EOK as i32 {
                error!("ext4_device_unregister: fail {}", r);
                return Err(r);
            }
        }

        debug!("lwext4 umount Okay");
        Ok(0)
    }

    pub fn lwext4_dir_ls(&self) -> Vec<String> {
        let path = &self.mount_point;
        let mut sss: [u8; 255] = [0; 255];
        let mut d: ext4_dir = unsafe { core::mem::zeroed() };

        let entry_to_str = |entry_type| match entry_type {
            EXT4_DE_UNKNOWN => "[unk] ",
            EXT4_DE_REG_FILE => "[file] ",
            EXT4_DE_DIR => "[dir] ",
            EXT4_DE_CHRDEV => "[chardev] ",
            EXT4_DE_BLKDEV => "[blk] ",
            EXT4_DE_FIFO => "[fifo] ",
            EXT4_DE_SOCK => "[sock] ",
            EXT4_DE_SYMLINK => "[sym] ",
            _ => "[???] ",
        };

        let mut res: Vec<String> = Vec::new();

        // info!("ls {}", str::from_utf8(path).unwrap());
        unsafe {
            ext4_dir_open(&mut d, path as *const _ as *const c_char);
            let mut de = ext4_dir_entry_next(&mut d);
            while !de.is_null() {
                let dentry = &(*de);
                sss.copy_from_slice(&dentry.name);
                sss[dentry.name_length as usize] = 0;

                // info!(
                //     "  {}{}",
                //     entry_to_str(dentry.inode_type as u32),
                //     str::from_utf8(&sss).unwrap()
                // );
                res.push(format!(
                    "{} {}",
                    entry_to_str(dentry.inode_type as u32),
                    core::str::from_utf8(&sss).unwrap()
                ));
                de = ext4_dir_entry_next(&mut d);
            }
            ext4_dir_close(&mut d);
        }
        res
        // info!("");
    }

    pub fn ext4_set_debug(&self) {
        unsafe {
            ext4_dmask_set(DEBUG_ALL);
        }
    }

    pub fn get_lwext4_mp_stats(&self) -> ext4_mount_stats {
        let mut stats: ext4_mount_stats = unsafe { core::mem::zeroed() };
        let c_mountpoint = &self.mount_point as *const _ as *const c_char;
        unsafe {
            ext4_mount_point_stats(c_mountpoint, &mut stats);
        }
        stats
    }

    pub fn print_lwext4_mp_stats(&self) {
        //struct ext4_mount_stats stats;
        let mut stats: ext4_mount_stats = unsafe { core::mem::zeroed() };

        let c_mountpoint = &self.mount_point as *const _ as *const c_char;

        unsafe {
            ext4_mount_point_stats(c_mountpoint, &mut stats);
        }

        debug!("********************");
        debug!("ext4_mount_point_stats");
        debug!("inodes_count = {:x?}", stats.inodes_count);
        debug!("free_inodes_count = {:x?}", stats.free_inodes_count);
        debug!("blocks_count = {:x?}", stats.blocks_count);
        debug!("free_blocks_count = {:x?}", stats.free_blocks_count);
        debug!("block_size = {:x?}", stats.block_size);
        debug!("block_group_count = {:x?}", stats.block_group_count);
        debug!("blocks_per_group= {:x?}", stats.blocks_per_group);
        debug!("inodes_per_group = {:x?}", stats.inodes_per_group);

        let vol_name = unsafe { core::ffi::CStr::from_ptr(&stats.volume_name as _) };
        debug!("volume_name = {:?}", vol_name);
        debug!("********************\n");
    }

    pub fn print_lwext4_block_stats(&self) {
        let ext4dev = unsafe { &*self.value.get() };
        //if ext4dev.is_null { return; }

        debug!("********************");
        debug!("ext4 blockdev stats");
        unsafe {
            debug!("bdev->bread_ctr = {:?}", (*ext4dev.bdif).bread_ctr);
            debug!("bdev->bwrite_ctr = {:?}", (*ext4dev.bdif).bwrite_ctr);

            debug!("bcache->ref_blocks = {:?}", (*ext4dev.bc).ref_blocks);
            debug!(
                "bcache->max_ref_blocks = {:?}",
                (*ext4dev.bc).max_ref_blocks
            );
            debug!("bcache->lru_ctr = {:?}", (*ext4dev.bc).lru_ctr);
        }
        debug!("********************\n");
    }
}

impl<K: KernelDevOp> Drop for Ext4BlockWrapper<K> {
    fn drop(&mut self) {
        info!("Drop struct Ext4BlockWrapper");
        self.lwext4_umount().unwrap();
        let devtype = unsafe {
            Box::from_raw((*self.value.get()).bdif.as_ref().unwrap().p_user as *mut K::DevType)
        };
        drop(devtype);
    }
}
