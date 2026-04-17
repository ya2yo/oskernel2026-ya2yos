use crate::*;

/// 参数可以是"brk\0"等
pub fn run_basic_musl(path: &str) {
    let args = [path];
    fork_and_run("/musl/basic\0", &args);
}

pub fn run_basic_glibc(path: &str) {
    let args = [path];
    fork_and_run("/glibc/basic\0", &args);
}

pub static ALL_BASIC: [&str; 32] = [
    "brk\0",
    "chdir\0",
    "clone\0",
    "close\0",
    "dup2\0",
    "dup\0",
    "execve\0",
    "exit\0",
    "fork\0",
    "fstat\0",
    "getcwd\0",
    "getdents\0",
    "getpid\0",
    "getppid\0",
    "gettimeofday\0",
    "mkdir_\0",
    "mmap\0",
    "mount\0",
    "munmap\0",
    "openat\0",
    "open\0",
    "pipe\0",
    "read\0",
    "sleep\0",
    "times\0",
    "umount\0",
    "uname\0",
    "unlink\0",
    "wait\0",
    "waitpid\0",
    "write\0",
    "yield\0",
];

static LA_BASIC_BLACKLIST: [&str; 3] = ["fstat\0", "mmap\0", "munmap\0"];

// ./busybox echo "#### OS COMP TEST GROUP START basic-musl ####"
// cd ./basic
// ./run-all.sh
// cd ..
// ./busybox echo "#### OS COMP TEST GROUP END basic-musl ####"

pub fn run_all_basic_musl_except_blacklist() {
    println!("#### OS COMP TEST GROUP START basic-musl ####");
    for program in basic::ALL_BASIC {
        if !LA_BASIC_BLACKLIST.contains(&program) {
            println!("Testing {} :", program);
            basic::run_basic_musl(program);
        }
    }
    println!("#### OS COMP TEST GROUP END basic-musl ####");
}

pub fn run_all_basic_glibc_except_blacklist() {
    println!("#### OS COMP TEST GROUP START basic-glibc ####");
    for program in basic::ALL_BASIC {
        if !LA_BASIC_BLACKLIST.contains(&program) {
            println!("Testing {} :", program);
            basic::run_basic_glibc(program);
        }
    }
    println!("#### OS COMP TEST GROUP END basic-glibc ####");
}
