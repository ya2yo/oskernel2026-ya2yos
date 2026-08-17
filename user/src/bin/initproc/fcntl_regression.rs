//! Focused regression for fcntl open-file-description and memfd semantics.

use core::arch::asm;
use user_lib::{close, fcntl, lseek, openat, println, read, write, OpenFlags};

const AT_FDCWD: isize = -100;
const SYS_MEMFD_CREATE: usize = 279;

const F_DUPFD: usize = 0;
const F_GETFD: usize = 1;
const F_SETFD: usize = 2;
const F_GETFL: usize = 3;
const F_SETFL: usize = 4;
const F_DUPFD_QUERY: usize = 1027;
const F_ADD_SEALS: usize = 1033;
const F_GET_SEALS: usize = 1034;
const F_GET_RW_HINT: usize = 1035;
const F_SET_RW_HINT: usize = 1036;
const F_GET_FILE_RW_HINT: usize = 1037;
const F_SET_FILE_RW_HINT: usize = 1038;

const FD_CLOEXEC: usize = 1;
const O_APPEND: usize = 0o2000;
const O_NONBLOCK: usize = 0o4000;
const MFD_ALLOW_SEALING: usize = 2;
const F_SEAL_SEAL: usize = 1;
const F_SEAL_SHRINK: usize = 2;
const F_SEAL_GROW: usize = 4;
const F_SEAL_WRITE: usize = 8;
const F_SEAL_FUTURE_WRITE: usize = 16;

#[cfg(target_arch = "riscv64")]
unsafe fn syscall3(number: usize, args: [usize; 3]) -> isize {
    let result: isize;
    asm!(
        "ecall",
        inlateout("a0") args[0] => result,
        in("a1") args[1],
        in("a2") args[2],
        in("a7") number,
    );
    result
}

#[cfg(target_arch = "loongarch64")]
unsafe fn syscall3(number: usize, args: [usize; 3]) -> isize {
    let result: isize;
    asm!(
        "syscall 0",
        inlateout("$a0") args[0] as usize => result,
        in("$a1") args[1] as usize,
        in("$a2") args[2] as usize,
        in("$a7") number,
    );
    result
}

unsafe fn memfd_create(name: &[u8], flags: usize) -> isize {
    syscall3(SYS_MEMFD_CREATE, [name.as_ptr() as usize, flags, 0])
}

fn close_all(fds: &[isize]) {
    for &fd in fds {
        if fd >= 0 {
            let _ = close(fd as usize);
        }
    }
}

pub fn run() -> bool {
    let mut fds = [
        -1isize, // memfd
        -1,      // plain dup
        -1,      // cloexec dup
        -1,      // independent memfd
        -1,      // first regular file open
        -1,      // second regular file open
        -1,      // regular-file dup
    ];
    let result = (|| -> Result<(), &'static str> {
        fds[0] = unsafe { memfd_create(b"fcntl-regression\0", MFD_ALLOW_SEALING) };
        if fds[0] < 0 {
            return Err("memfd_create");
        }
        if fcntl(fds[0] as usize, F_GET_SEALS, 0) != 0 {
            return Err("initial seals");
        }

        fds[1] = fcntl(fds[0] as usize, F_DUPFD, 0);
        if fds[1] < 0 || fcntl(fds[0] as usize, F_DUPFD_QUERY, fds[1] as usize) != 1 {
            return Err("dup/query");
        }
        if fcntl(fds[0] as usize, F_SETFD, FD_CLOEXEC) != 0
            || fcntl(fds[0] as usize, F_GETFD, 0) != FD_CLOEXEC as isize
            || fcntl(fds[1] as usize, F_GETFD, 0) != 0
        {
            return Err("descriptor cloexec isolation");
        }
        fds[2] = fcntl(fds[0] as usize, 1030, 0);
        if fds[2] < 0 || fcntl(fds[2] as usize, F_GETFD, 0) != FD_CLOEXEC as isize {
            return Err("dupfd cloexec");
        }

        if write(fds[0] as usize, b"ab", 2) != 2 {
            return Err("memfd initial write");
        }
        let flags = fcntl(fds[0] as usize, F_GETFL, 0);
        if flags < 0
            || fcntl(
                fds[0] as usize,
                F_SETFL,
                flags as usize | O_APPEND | O_NONBLOCK,
            ) != 0
            || fcntl(fds[1] as usize, F_GETFL, 0) as usize & (O_APPEND | O_NONBLOCK)
                != O_APPEND | O_NONBLOCK
        {
            return Err("shared status flags");
        }
        if lseek(fds[0] as usize, 0, 0) != 0 || write(fds[1] as usize, b"c", 1) != 1 {
            return Err("append write");
        }
        if lseek(fds[0] as usize, 0, 0) != 0 {
            return Err("memfd rewind");
        }
        let mut data = [0u8; 3];
        if read(fds[1] as usize, &mut data, 3) != 3 || &data != b"abc" {
            return Err("append contents");
        }

        fds[3] = unsafe { memfd_create(b"fcntl-independent\0", MFD_ALLOW_SEALING) };
        if fds[3] < 0
            || fcntl(fds[0] as usize, F_DUPFD_QUERY, fds[3] as usize) != 0
            || fcntl(fds[3] as usize, F_ADD_SEALS, F_SEAL_FUTURE_WRITE) != 0
            || fcntl(fds[3] as usize, F_GET_SEALS, 0) != F_SEAL_FUTURE_WRITE as isize
            || write(fds[3] as usize, b"x", 1) >= 0
        {
            return Err("independent query");
        }
        let seal = fcntl(fds[0] as usize, F_ADD_SEALS, F_SEAL_WRITE);
        if seal != 0
            || fcntl(fds[1] as usize, F_GET_SEALS, 0) != F_SEAL_WRITE as isize
            || write(fds[1] as usize, b"x", 1) >= 0
            || fcntl(fds[0] as usize, F_ADD_SEALS, F_SEAL_SEAL) != 0
            || fcntl(fds[0] as usize, F_ADD_SEALS, F_SEAL_GROW | F_SEAL_SHRINK) >= 0
        {
            return Err("memfd seals");
        }

        let path = "/tmp/fcntl-rw-hint\0";
        fds[4] = openat(
            AT_FDCWD,
            path,
            OpenFlags::O_CREATE | OpenFlags::O_TRUNC | OpenFlags::O_RDWR,
            0o600,
        );
        fds[5] = openat(AT_FDCWD, path, OpenFlags::O_RDWR, 0);
        if fds[4] < 0 || fds[5] < 0 {
            return Err("hint opens");
        }
        fds[6] = fcntl(fds[4] as usize, F_DUPFD, 0);
        let mut inode_hint = 4u64;
        let mut file_hint = 2u64;
        if fds[6] < 0
            || fcntl(
                fds[4] as usize,
                F_SET_RW_HINT,
                &mut inode_hint as *mut u64 as usize,
            ) != 0
            || fcntl(
                fds[5] as usize,
                F_GET_RW_HINT,
                &mut inode_hint as *mut u64 as usize,
            ) != 0
            || inode_hint != 4
            || fcntl(
                fds[4] as usize,
                F_SET_FILE_RW_HINT,
                &mut file_hint as *mut u64 as usize,
            ) != 0
            || fcntl(
                fds[6] as usize,
                F_GET_FILE_RW_HINT,
                &mut file_hint as *mut u64 as usize,
            ) != 0
            || file_hint != 2
        {
            return Err("rw hints");
        }

        let no_sealing = unsafe { memfd_create(b"fcntl-no-sealing\0", 0) };
        if no_sealing < 0 {
            return Err("no-sealing memfd");
        }
        let no_seal_state = fcntl(no_sealing as usize, F_GET_SEALS, 0);
        let no_seal_add = fcntl(no_sealing as usize, F_ADD_SEALS, F_SEAL_SEAL);
        let _ = close(no_sealing as usize);
        if no_seal_state != F_SEAL_SEAL as isize || no_seal_add >= 0 {
            return Err("no-sealing policy");
        }

        Ok(())
    })();

    close_all(&fds);
    match result {
        Ok(()) => {
            println!("fcntl regression: PASS");
            true
        }
        Err(step) => {
            println!("fcntl regression: FAIL ({})", step);
            false
        }
    }
}
