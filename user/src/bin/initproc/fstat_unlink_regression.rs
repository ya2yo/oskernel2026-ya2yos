use user_lib::{
    close, exit, fork, fstat, openat, pipe, println, read, unlink, waitpid, write, OpenFlags,
};

const AT_FDCWD: isize = -100;
const PATH: &str = "/tmp/fstat-unlink-regression\0";

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

pub fn run() -> bool {
    let result = (|| -> Result<(), &'static str> {
        let _ = unlink(AT_FDCWD, PATH, OpenFlags::empty());
        let initial_fd = openat(
            AT_FDCWD,
            PATH,
            OpenFlags::O_CREATE | OpenFlags::O_TRUNC | OpenFlags::O_RDWR,
            0o600,
        );
        if initial_fd < 0 {
            return Err("create");
        }
        if write(initial_fd as usize, b"stat", 4) != 4 || close(initial_fd as usize) != 0 {
            return Err("initialize");
        }

        let mut child_ready = [0u32; 2];
        let mut parent_ready = [0u32; 2];
        if pipe(&mut child_ready, 0) != 0 || pipe(&mut parent_ready, 0) != 0 {
            return Err("pipe");
        }

        let child = fork();
        if child < 0 {
            return Err("fork");
        }
        if child == 0 {
            let _ = close(child_ready[0] as usize);
            let _ = close(parent_ready[1] as usize);
            let fd = openat(AT_FDCWD, PATH, OpenFlags::O_RDONLY, 0);
            if fd < 0 {
                exit(1);
            }
            if write(child_ready[1] as usize, &[1], 1) != 1 {
                exit(2);
            }
            let mut release = [0u8; 1];
            if read(parent_ready[0] as usize, &mut release, 1) != 1 {
                exit(3);
            }
            let mut stat = KernelKstat::default();
            let ret = fstat(fd as usize, stat.as_bytes_mut());
            let _ = close(fd as usize);
            exit(if ret == 0 && stat.st_ino != 0 && stat.st_size == 4 {
                0
            } else {
                4
            });
        }

        let _ = close(child_ready[1] as usize);
        let _ = close(parent_ready[0] as usize);
        let mut ready = [0u8; 1];
        if read(child_ready[0] as usize, &mut ready, 1) != 1 {
            return Err("child open handshake");
        }
        if unlink(AT_FDCWD, PATH, OpenFlags::empty()) != 0 {
            return Err("unlink");
        }
        if write(parent_ready[1] as usize, &[1], 1) != 1 {
            return Err("release child");
        }
        let mut status = 0;
        if waitpid(child as usize, &mut status) != child || status != 0 {
            return Err("fstat after cross-process unlink");
        }
        Ok(())
    })();

    match result {
        Ok(()) => {
            println!("fstat unlink regression: PASS");
            true
        }
        Err(step) => {
            println!("fstat unlink regression: FAIL ({})", step);
            false
        }
    }
}
