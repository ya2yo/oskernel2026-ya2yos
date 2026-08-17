use user_lib::{close, fstat, mkdir, openat, println, renameat2, write, OpenFlags};

const AT_FDCWD: isize = -100;
const OLD_DIR: &str = "/tmp/fstat-rename-subtree-old\0";
const NEW_DIR: &str = "/tmp/fstat-rename-subtree-new\0";
const OLD_FILE: &str = "/tmp/fstat-rename-subtree-old/query-cache.bin\0";

#[repr(C)]
#[derive(Default)]
struct KernelKstat {
    st_dev: usize,
    st_ino: usize,
    st_mode: u32,
    st_nlink: u32,
    st_uid: u32,
    st_gid: u32,
    st_rdev: usize,
    pad: usize,
    st_size: isize,
    st_blksize: i32,
    pad2: u32,
    st_blocks: isize,
    st_atime: usize,
    st_atime_nsec: usize,
    st_mtime: usize,
    st_mtime_nsec: usize,
    st_ctime: usize,
    st_ctime_nsec: usize,
    unused: [u32; 2],
}

impl KernelKstat {
    fn as_bytes_mut(&mut self) -> &mut [u8] {
        unsafe {
            core::slice::from_raw_parts_mut(
                self as *mut Self as *mut u8,
                core::mem::size_of::<Self>(),
            )
        }
    }
}

/// An fd below a renamed directory must remain attached to the same inode.
/// Rustc commits incremental artifacts by renaming `s-*-working` directories,
/// which otherwise leaves Ya2yOS's pathname-backed Ext4File at the old path.
pub fn run() -> bool {
    let result = (|| -> Result<(), &'static str> {
        if mkdir(AT_FDCWD, OLD_DIR, 0o700) != 0 {
            return Err("mkdir source");
        }
        let fd = openat(
            AT_FDCWD,
            OLD_FILE,
            OpenFlags::O_CREATE | OpenFlags::O_TRUNC | OpenFlags::O_RDWR,
            0o600,
        );
        if fd < 0 {
            return Err("create child");
        }
        if write(fd as usize, b"stat", 4) != 4 {
            let _ = close(fd as usize);
            return Err("write child");
        }
        if renameat2(AT_FDCWD, OLD_DIR, AT_FDCWD, NEW_DIR, 0) != 0 {
            let _ = close(fd as usize);
            return Err("rename parent");
        }

        let mut stat = KernelKstat::default();
        let fstat_result = fstat(fd as usize, stat.as_bytes_mut());
        let _ = close(fd as usize);
        if fstat_result != 0 || stat.st_ino == 0 || stat.st_size != 4 {
            return Err("fstat after parent rename");
        }
        Ok(())
    })();

    match result {
        Ok(()) => {
            println!("fstat rename subtree regression: PASS");
            true
        }
        Err(step) => {
            println!("fstat rename subtree regression: FAIL ({})", step);
            false
        }
    }
}
