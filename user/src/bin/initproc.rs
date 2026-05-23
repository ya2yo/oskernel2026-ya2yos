#![no_std]
#![no_main]
#![allow(unused_imports)]
#![allow(unused_variables)]

extern crate user_lib;

use libctest::runall::{run_specific_test, runall};
use user_lib::{
    AF_INET, SOCK_DGRAM, SOCK_STREAM, chdir, execve, exit, fork, println, run_busyboxsh, run_libc_bench, run_lmbench_test, shutdown, socket, wait, waitpid
};

mod basic;
mod libctest;
mod lmbench;
mod ltp;
mod lua;

#[allow(dead_code)]
/// fork，并在子进程中运行一个testsuit
fn run_testsuit(root: &str, script: &str) {
    let args = ["busybox\0", "sh\0", script];
    fork_and_run(root, &args);
}

#[allow(dead_code)]
fn test_cgroup_fj_function_cpuset_via_script() {
    let args = [
        "/musl/busybox\0",
        "sh\0",
        "-c\0",
        "PATH=/musl/ltp/testcases/bin:/bin:$PATH; export PATH; ./cgroup_fj_function.sh cpuset\0",
    ];
    println!("#### OS COMP TEST GROUP START ltp-musl-cgroup-fj-cpuset ####");
    fork_and_run("/musl/ltp/testcases/bin\0", &args);
    println!("#### OS COMP TEST GROUP END ltp-musl-cgroup-fj-cpuset ####");
}

pub fn fork_and_run(dir: &str, args: &[&str]) -> i32 {
    println!("{:?}", args);
    let pid = fork();
    if pid == 0 {
        // 子进程
        chdir(dir);
        let ret = execve(&args);
        println!("execve fail!");
        exit(0);
    } else {
        // 父进程
        let mut exit_code: i32 = 0;
        let _ = waitpid(pid as usize, &mut exit_code);
        return exit_code;
    }
}

const LTP_TEST_START: usize = 0;
const LTP_TESTS_PER_GROUP: usize = 1;

fn trim_trailing_nul(s: &str) -> &str {
    let bytes = s.as_bytes();
    let mut end = bytes.len();
    if end > 0 && bytes[end - 1] == 0 {
        end -= 1;
    }
    unsafe { core::str::from_utf8_unchecked(&bytes[..end]) }
}

#[allow(unused)]
fn run_ltp_tests_musl_separately(tests: &[&str], blacklist: &[&str]) {
    let mut group = 0;
    let mut i = 0;
    while i < tests.len() {
        let group_start = i;
        let mut group_end = i + LTP_TESTS_PER_GROUP;
        if group_end > tests.len() {
            group_end = tests.len();
        }

        println!(
            "#### OS COMP TEST GROUP START ltp-musl-{}-{} ####",
            group,
            LTP_TEST_START + group_start
        );

        let mut j = group_start;
        while j < group_end {
            let test = tests[j];
            if blacklist.contains(&test) {
                println!("SKIP LTP CASE {}", trim_trailing_nul(test));
                j += 1;
                continue;
            }
            println!("RUN LTP CASE {}", test);
            let r = fork_and_run("/musl/ltp/testcases/bin\0", &[test]);
            println!("FAIL LTP CASE {} : {}", test, r); // 这不是表示失败了，只是告诉外界程序返回值是多少
            j += 1;
        }

        println!(
            "#### OS COMP TEST GROUP END ltp-musl-{}-{} ####",
            group,
            LTP_TEST_START + group_start
        );
        group += 1;
        i = group_end;
    }
}

#[no_mangle]
#[cfg(target_arch = "loongarch64")]
fn main() -> i32 {
    println!("initproc running......");
    basic::run_all_basic_musl_except_blacklist();
    basic::run_all_basic_glibc_except_blacklist();
    lua::run_all_lua_musl();

    println!("#### OS COMP TEST GROUP START libctest-musl ####");
    libctest::runall::runall(
        "/musl\0",
        "entry-static.exe\0",
        &[
            "pthread_cancel_points\0",
            "pthread_cancel\0",
            "pthread_cond\0",
            "pthread_tsd\0",
            "stat\0",
            "utime\0",
            "pthread_robust_detach\0",
            "pthread_cancel_sem_wait\0",
            "pthread_cond_smasher\0",
            "pthread_exit_cancel\0",
            "pthread_once_deadlock\0",
            "pthread_rwlock_ebusy\0",
        ],
    );
    libctest::runall::runall(
        "/musl\0",
        "entry-dynamic.exe\0",
        &[
            "pthread_cancel_points\0",
            "pthread_cancel\0",
            "pthread_cond\0",
            "pthread_tsd\0",
            "stat\0",
            "utime\0",
            "pthread_robust_detach\0",
            "pthread_cancel_sem_wait\0",
            "pthread_cond_smasher\0",
            "pthread_exit_cancel\0",
            "pthread_once_deadlock\0",
            "pthread_rwlock_ebusy\0",
            "daemon_failure\0",
            "fflush_exit\0",
        ],
    );
    println!("#### OS COMP TEST GROUP END libctest-musl ####");
    shutdown();
    return 0;
}
#[allow(unused)]
fn test_ltp() {
    // 每个LTP case单独作为一个测试组运行，避免单组日志超过1万行。
    // 如需从中间恢复，修改LTP_TEST_START即可。
    let test = &ltp::FILELIST[LTP_TEST_START..];
    // 7号存在问题
    run_ltp_tests_musl_separately(
        test,
        &[
            // [100,200)区间
            // cgroup_fj系列需要带参数的脚本入口，直接跑helper会卡死。
            // 需要验证时使用test_cgroup_fj_function_cpuset_via_script。
            "cgroup_fj_common.sh\0",
            "cgroup_fj_function.sh\0",
            "cgroup_fj_proc\0",
            "cgroup_fj_stress.sh\0",
            "cgroup_lib.sh\0",
            "cgroup_regression_3_1.sh\0",         // mkdir: can't create directory '/0': File exists
            "cgroup_regression_3_2.sh\0", // cat: can't open '/proc/sched_debug': No such file or directory
            "cgroup_regression_5_1.sh\0", // 卡死
            "cgroup_regression_5_2.sh\0", // 卡死
            "cgroup_regression_6_1.sh\0", // 卡死
            "cgroup_regression_6_2.sh\0", // 卡死
            "cgroup_regression_fork_processes\0", // 卡死
            "cgroup_regression_getdelays\0", // socket(domain=Netlink)
            "clock_nanosleep01\0",        // 卡死
            "clock_nanosleep04\0",        // 卡死
            "clone02\0",                  // translate_va失败？
            "clone03\0", // [kernel] Panicked at src/mm/memory_set.rs:1241 called `Option::unwrap()` on a `None` value
            "clone08\0", // [kernel] Panicked at src/task/task/process.rs:151 process[4] removed but still refed! refcnt=2
            "connect01\0", // [kernel] Panicked at src/fs/vfs.rs:129 not implemented
            "cpuctl_fj_cpu-hog\0", // 卡死
            // [200,300)区间
            "cpufreq_boost\0",  // panic
            "crash02\0", // [kernel] Panicked at src/mm/translate.rs:106 called `Option::unwrap()` on a `None` value
            "creat06\0", // get_proc_by_hartid: fail because hartid=29 is too large! + error:ext4_fopen:
            "creat07\0", // [kernel] Panicked at src/mm/translate.rs:212 called `Option::unwrap()` on a `None` value
            "cve-2017-17052\0", // [kernel] Panicked at src/mm/memory_set.rs:1241 called `Option::unwrap()` on a `None` value
            "dio_append\0", // [kernel] Panicked at src/mm/memory_set.rs:591 called `Option::unwrap()` on a `None` value
            "dio_read\0", // [kernel] Panicked at src/mm/memory_set.rs:1241 called `Option::unwrap()` on a `None` value
            "dio_sparse\0", // [kernel] Panicked at src/mm/frame_alloc/buddy_cma.rs:63 called `Result::unwrap()` on an `Err` value: ()
            "dio_truncate\0", // [kernel] Panicked at src/mm/memory_set.rs:591 called `Option::unwrap()` on a `None` value
            "diotest4\0", // [kernel] Panicked at src/trap/mod.rs:170 Unsupported trap Unknown, stval = 0x0!
            "diotest6\0", // 卡死
            "dirty\0",    // 时间较长 + warn
            "dirtyc0w\0", // 时间较长 + error
            "dirtyc0w_shmem\0", // 卡死 + error：Unsupported syscall_id: 144, kernel exit this process with exitcode=-1!
            "dirtypipe\0", // [kernel] Panicked at src/syscall/fs.rs:783 called `Option::unwrap()` on a `None` value
            "doio\0",      // 卡死
            // [300,400)区间
            "epoll_wait05\0", // [kernel] Panicked at src/mm/memory_set.rs:1241 called `Option::unwrap()` on a `None` value
            "execve02\0", // [kernel] Panicked at src/mm/translate.rs:212 called `Option::unwrap()` on a `None` value
            "execve04\0", // [kernel] Panicked at src/mm/translate.rs:212 called `Option::unwrap()` on a `None` value
            "execve05\0", // [kernel] Panicked at src/mm/translate.rs:212 called `Option::unwrap()` on a `None` value
            "execveat01\0", // [kernel] Panicked at src/mm/translate.rs:212 called `Option::unwrap()` on a `None` value
            "execveat02\0", // [kernel] Panicked at src/mm/translate.rs:212 called `Option::unwrap()` on a `None` value
            "exit_group01\0", // [kernel] Panicked at src/mm/memory_set.rs:1241 called `Option::unwrap()` on a `None` value
            "fanotify12\0", // [kernel] Panicked at src/mm/translate.rs:212 called `Option::unwrap()` on a `None` value
            // [400,500)区间
            "fcntl13\0", // [kernel] Panicked at src/syscall/fs.rs:783 called `Option::unwrap()` on a `None` value
            "fcntl13_64\0", // [kernel] Panicked at src/syscall/fs.rs:783 called `Option::unwrap()` on a `None` value
            "fcntl14\0",    // 时间较长 + error
            "fcntl14_64\0", // 时间较长 + error
            "fcntl34\0", //[kernel] Panicked at src/syscall/fs.rs:783 called `Option::unwrap()` on a `None` value
            "fcntl34_64\0", //[kernel] Panicked at src/syscall/fs.rs:783 called `Option::unwrap()` on a `None` value
            "fcntl35\0", //[kernel] Panicked at src/syscall/fs.rs:783 called `Option::unwrap()` on a `None` value
            "fcntl36\0", // 同上
            "fcntl36_64\0", // 同上
            "fcntl37\0", // 同上
            "fcntl37_64\0", // 同上
            // [500,600)区间
            "flock03\0", // [kernel] Panicked at src/task/task/process.rs:151 process[3] removed but still refed! refcnt=2
            "force_erase.sh\0", // 需输入y/n
            "fork04\0",  // 卡死
            "fork07\0",  // 卡死
            "fork14\0", // 卡死 warn：Seek beyond the end of the file,path is /tmp/LTP_forGbdpEK/ltp_fork14_2,offset is 16228352 while size is 929792
            "fork_exec_loop\0", //[kernel] Panicked at /home/tatlin-os/lwext4_rust/src/ulibc.rs:92 malloc failed
            // [600,700)区间
            "fs_racer_dir_test.sh\0",  // 卡死
            "fs_racer_file_list.sh\0", // 卡死
            "fstat02\0", // [kernel] Panicked at src/syscall/fs.rs:441 not yet implemented
            "fstat02_64\0", // [kernel] Panicked at src/syscall/fs.rs:441 not yet implemented
            "fstat03_64\0", // [kernel] Panicked at src/fs/fstruct.rs:163 index out of bounds: the len is 5 but the index is 100
            "fstatat01\0", // [kernel] Panicked at src/fs/fstruct.rs:163 index out of bounds: the len is 5 but the index is 100
            // [700,800)区间
            "futex_cmp_requeue01\0", // [kernel] Panicked at src/task/futex.rs:280 not implemented
            "futex_cmp_requeue02\0", // [kernel] Panicked at src/task/futex.rs:280 not implemented
            "futex_wait02\0", // [kernel] Panicked at src/mm/memory_set.rs:1241 called `Option::unwrap()` on a `None` value
            "futex_wait04\0", // [kernel] Panicked at src/task/processor.rs:56 get_proc_by_hartid: fail because hartid=18446743800982615104 is too large!
            "futex_wake03\0", // 卡死
            "genfrexp\0",     // 卡死
            "genhypot\0",     // 卡死
            "genmodf\0",      // 卡死
            // [800,900)区间
            "getpid02\0", // [kernel] Panicked at src/mm/memory_set.rs:1241 called `Option::unwrap()` on a `None` value
            "getrusage03\0", // [kernel] Panicked at src/mm/translate.rs:212 called `Option::unwrap()` on a `None` value
            "getrusage04\0", // [kernel] Panicked at src/mm/translate.rs:212 called `Option::unwrap()` on a `None` value
            "getsockopt02\0", // [kernel] Panicked at src/task/task/process.rs:151 process[3] removed but still refed! refcnt=2
            "growfiles\0", // [kernel] Panicked at src/task/task/process.rs:151 process[2] removed but still refed! refcnt=2
            "hackbench\0", // [kernel] Panicked at src/task/task/process.rs:140 called `Option::unwrap()` on a `None` value
            // [900,1000)区间
            "in6_02\0", // [kernel] Panicked at src/fs/files/stdio.rs:90 called `Result::unwrap()` on an `Err` value: Utf8Error { valid_up_to: 59, error_len: Some(1) }
            "inode01\0", // 卡死
            "inode02\0", // 卡死
            // [1000,---)区间
            "ioctl_ns05\0", // [kernel] Panicked at src/task/task/process.rs:151 process[3] removed but still refed! refcnt=2
            "ioctl_ns06\0", // [kernel] Panicked at src/task/task/process.rs:151 process[3] removed but still refed! refcnt=2
            "kill02\0",     // 卡死
            "kill05\0", // [kernel] Panicked at src/task/task/process.rs:151 process[4] removed but still refed! refcnt=2
            "kill06\0", // [kernel] Panicked at src/task/task/process.rs:151 process[4] removed but still refed! refcnt=6
            "kill08\0", // 同上
            "kill09\0", // 卡死
            "kill10\0", // 卡死
            "leapsec01\0", // 卡死
            // [1100,---)区间
            "link02\0", // [kernel] Panicked at src/syscall/fs.rs:441 not yet implemented
            "link04\0", // 同上
            "link05\0", // 同上
            "link08\0", // 同上
            "madvise05\0", // [kernel] Panicked at src/syscall/memory.rs:34 sys_mmap: Failed to convert flags to MmapFlags bitmap: value is 0x8022
            "mallocstress\0", // [kernel] Panicked at src/utils/simple_range.rs:22 start VPN:0xfffffff87bac0 > end VPN:0x4e5921!
            // [1200,---)区间
            "memcg_test_2\0",      // 卡死
            "memcg_test_4\0",      // 卡死
            "memcg_test_4.sh\0",   // 卡死
            "mlockall03\0",        // 卡死
            "mmap-corruption01\0", // [kernel] Panicked at src/mm/frame_alloc/buddy_cma.rs:63 called `Result::unwrap()` on an `Err` value: ()
            "mmap001\0",           // 卡死
            "mmap01\0", // [kernel] Panicked at src/trap/mod.rs:170 Unsupported trap Unknown, stval = 0x2683ffff!
            "mmap12\0", // [kernel] Panicked at src/syscall/memory.rs:34 sys_mmap: Failed to convert flags to MmapFlags bitmap: value is 0x8002
            "mmap15\0", // [kernel] Panicked at src/utils/simple_range.rs:22 start VPN:0xfffffffffffff > end VPN:0x0!
            "mmap17\0", // [kernel] Panicked at src/syscall/memory.rs:34 sys_mmap: Failed to convert flags to MmapFlags bitmap: value is 0x100002
            "mmap18\0", // [kernel] Panicked at src/syscall/memory.rs:34 sys_mmap: Failed to convert flags to MmapFlags bitmap: value is 0x132
            "mmap20\0", // [kernel] Panicked at src/syscall/memory.rs:34 sys_mmap: Failed to convert flags to MmapFlags bitmap: value is 0x403
            "mmapstress01\0", // 卡死
            // [1300,---)区间
            "mprotect02\0",  // ks
            "mprotect03\0",  // ks
            "mprotect04\0",  // ks
            "mremap01\0",    // ks
            "mremap02\0", // [kernel] Panicked at src/syscall/memory.rs:138 called `Option::unwrap()` on a `None` value
            "mremap03\0", // [kernel] Panicked at src/syscall/memory.rs:138 called `Option::unwrap()` on a `None` value
            "mremap04\0", // ts
            "mremap05\0", // [kernel] Panicked at src/syscall/memory.rs:127 fixed && !may_mov
            "mremap06\0", // [kernel] Panicked at src/syscall/memory.rs:138 called `Option::unwrap()` on a `None` value
            "msync02\0", // [kernel] Panicked at src/syscall/memory.rs:34 sys_mmap: Failed to convert flags to MmapFlags bitmap: value is 0x2001
            "msync03\0", // ts
            "mtest01\0", // [kernel] Panicked at src/mm/frame_alloc/buddy_cma.rs:63 called `Result::unwrap()` on an `Err` value: ()
            "munlock02\0", // ks
            "munmap02\0", // ks
            "munmap03\0", // [kernel] Panicked at src/mm/address.rs:249 assertion `left == right` failed
            "nanosleep04\0", // ks
            // [1400,---)区间
            "netstress\0", // 需要使用的和网络相关内容太多
            "nice05\0",    // ks
            "nptl01\0",    // 耗时较长 但success ---------------------
            "open11\0",    // [kernel] Panicked at src/syscall/fs.rs:441 not yet implemented
            "openat01\0", // [kernel] Panicked at src/fs/fstruct.rs:163 index out of bounds: the len is 6 but the index is 100
            "openfile\0", // ks
            "page01\0", // [kernel] Panicked at src/task/task/process.rs:140 called `Option::unwrap()` on a `None` value
            "pause01\0", // 耗时较长 + error
            "pause02\0", // process[2] removed but still refed! refcnt=2
            // [1500,---)区间
            "pidns32\0", // [kernel] Panicked at src/mm/memory_set.rs:1241 called `Option::unwrap()` on a `None` value
            "pipe11\0",  // ks
            "pipe12\0", // [kernel] Panicked at src/syscall/fs.rs:783 called `Option::unwrap()` on a `None` value
            "pipe15\0", // [kernel] Panicked at src/syscall/fs.rs:783 called `Option::unwrap()` on a `None` value
            "pipe2_02\0", // [kernel] Panicked at src/mm/translate.rs:212 called `Option::unwrap()` on a `None` value
            "poll01\0", // [kernel] Panicked at src/mm/translate.rs:145 called `Option::unwrap()` on a `None` value
            "ppoll01\0", // [kernel] Panicked at src/mm/translate.rs:145 called `Option::unwrap()` on a `None` value
            // [1600,---)区间
            "prot_hsymlinks\0", // [kernel] Panicked at src/mm/translate.rs:212 called `Option::unwrap()` on a `None` value
            "pselect02\0",      // ks
            "pselect02_64\0",   // ks
            "pthcli\0",         // ks
            "pthserv\0",        // ks
            "readlinkat02\0", // [kernel] Panicked at src/fs/fstruct.rs:163 index out of bounds: the len is 5 but the index is 18446744073709551615
            "readv02\0",      // ks
            "recv01\0",       // [kernel] Panicked at src/fs/vfs.rs:129 not implemented
            "recvfrom01\0",   // ts
            "recvmsg01\0", // [kernel] Panicked at src/task/task/process.rs:151 process[3] removed but still refed! refcnt=2
            "recvmsg03\0", // [kernel] Panicked at src/task/task/process.rs:151 process[3] removed but still refed! refcnt=2
            // [1700,---)区间
            "rmdir02\0",              // ks
            "rt_sigaction02\0", // [kernel] Panicked at src/mm/translate.rs:212 called `Option::unwrap()` on a `None` value
            "rt_sigprocmask02\0", // ts
            "rt_sigqueueinfo01\0", // ks
            "rt_sigsuspend01\0", // ks
            "run_sched_cliserv.sh\0", // [kernel] Panicked at src/task/task/process.rs:151 process[2] removed but still refed! refcnt=2
            "sched_driver\0", // [kernel] Panicked at src/mm/translate.rs:212 called `Option::unwrap()` on a `None` value
            "sched_getaffinity01\0", // ks
            "sched_getattr01\0", // ks
            // [1800,---)区间
            "select03\0", // [kernel] Panicked at src/task/processor.rs:56 get_proc_by_hartid: fail because hartid=18446743800981759942 is too large!
            "select04\0", // ks
            "semtest_2ns\0", // [kernel] Panicked at src/task/task/process.rs:151 process[5] removed but still refed! refcnt=2
            "send01\0", // [kernel] Panicked at src/task/task/process.rs:151 process[2] removed but still refed! refcnt=2
            "sendfile04\0", // ks
            "sendfile04_64\0", // ks
            "sendmsg01\0", // [kernel] Panicked at src/trap/mod.rs:170 Unsupported trap Unknown, stval = 0x9000!
            "sendto01\0", // [kernel] Panicked at src/task/task/process.rs:151 process[2] removed but still refed! refcnt=2
            "setfsgid03\0", // ks
            "setfsgid03_16\0", // ks
            "setitimer01\0", // [kernel] Panicked at src/syscall/time.rs:47 only support Itimer Real
            "setitimer02\0", // [kernel] Panicked at src/mm/translate.rs:212 called `Option::unwrap()` on a `None` value
            // [1900,---)区间
            "setpgid03\0", // [kernel] Panicked at src/task/futex.rs:204 called `Option::unwrap()` on a `None` value
            "setpriority01\0", // [kernel] Panicked at src/task/task/process.rs:151 process[3] removed but still refed! refcnt=2
            "setrlimit05\0", // [kernel] Panicked at src/task/processor.rs:56 get_proc_by_hartid: fail because hartid=18446744073709551615 is too large!
            "setrlimit06\0", // ks
            "shm_test\0",    // ks
            "shmat03\0", // [kernel] Panicked at src/syscall/memory.rs:251 called `Option::unwrap()` on a `None` value
            "shmat04\0", // [kernel] Panicked at src/syscall/memory.rs:275 [sys_shmctl] unsupport cmd
            "shmctl01\0", // [kernel] Panicked at src/syscall/memory.rs:275 [sys_shmctl] unsupport cmd  +  clockid == 5
            "shmctl03\0", // [kernel] Panicked at src/syscall/memory.rs:275 [sys_shmctl] unsupport cmd
            "shmctl04\0", // ts
            "shmctl06\0", // ts
            "shmctl07\0", // ts
            "shmctl08\0", // ts
            // [2000,---)区间
            "shmt04\0", // [kernel] Panicked at src/mm/memory_set.rs:483 [shm_attach] unimplement attach addr
            "shmt05\0", // [kernel] Panicked at src/mm/memory_set.rs:483 [shm_attach] unimplement attach addr
            "shmt10\0", // [kernel] Panicked at src/task/task/process.rs:151 process[2] removed but still refed! refcnt=2
            "sighold02\0", // ks
            "sigrelse01\0", // [kernel] Panicked at src/task/task/process.rs:151 process[2] removed but still refed! refcnt=2
            "sigsuspend01\0", // ks
            "splice02\0", // [kernel] Panicked at src/syscall/fs.rs:783 called `Option::unwrap()` on a `None` value
            "starvation\0", // ks
            "stat03\0",   // ks
            "stat03_64\0", // ks
            // [2100,---)区间
            "symlink03\0", // [kernel] Panicked at src/mm/translate.rs:106 called `Option::unwrap()` on a `None` value
            "sysctl03\0", // [kernel] Panicked at src/task/task/process.rs:151 process[3] removed but still refed! refcnt=2
            "sysinfo01\0", // [kernel] Panicked at src/mm/translate.rs:212 called `Option::unwrap()` on a `None` value
            "sysinfo02\0", // ts
            // [2200,---)区间
            // -- 无，网络部分
            // [2300,---)区间
            // -- 无，网络部分
            // [2400,---)区间
            // -- 无，网络部分
            // [2500,---)区间
            "tgkill01\0",       // ks
            "tgkill02\0", // [kernel] Panicked at src/signal/signal.rs:125 called `Option::unwrap()` on a `None` value
            "tgkill03\0", // ts
            "thp01\0", // [kernel] Panicked at src/mm/memory_set.rs:1241 called `Option::unwrap()` on a `None` value
            "timed_forkbomb\0", // ks
            "times03\0", // ks
            // [2600,---)区间
            "tst_hexdump\0",      // ks
            "tst_supported_fs\0", // [kernel] Panicked at src/mm/translate.rs:212 called `Option::unwrap()` on a `None` value
            "umask01\0",          // 耗时较长 + error
            "uname02\0",          // ks
            // [2700,---)区间
            "unlink07\0",   // ks
            "unlinkat01\0", // [kernel] Panicked at src/fs/fstruct.rs:163 index out of bounds: the len is 5 but the index is 100
            "utsname01\0", // [kernel] Panicked at src/mm/memory_set.rs:1241 called `Option::unwrap()` on a `None` value
            "utsname02\0", // ts
            "utsname03\0", // ts
            "vma01\0",     // ks
            "vmsplice04\0", // [kernel] Panicked at src/syscall/fs.rs:783 called `Option::unwrap()` on a `None` value
            "waitid01\0", // [kernel] Panicked at src/task/task/process.rs:151 process[3] removed but still refed! refcnt=2
            "waitid04\0", // ts
            "waitid05\0", // ts
            "waitid06\0", // ts
            "waitid07\0", // ts
            "waitid08\0", // ts
            "waitid09\0", // ts
            "waitid11\0", // ts
            "waitpid04\0", // [kernel] Panicked at src/syscall/process.rs:275 [sys_wait4] We cannot handle input.pid<-1 (input.pid=-2147483648)
            "waitpid06\0", // [kernel] Panicked at src/mm/memory_set.rs:1241 called `Option::unwrap()` on a `None` value
            "waitpid07\0", // ks
            "waitpid08\0", // ks
            "waitpid09\0", // ks
            "waitpid10\0", // [kernel] Panicked at src/mm/memory_set.rs:1241 called `Option::unwrap()` on a `None` value
            "waitpid11\0", // ts
            "waitpid12\0", // ks
            "waitpid13\0", // ks
            // [2800,---)区间
            "writev01\0", // [kernel] Panicked at src/syscall/fs.rs:123 called `Option::unwrap()` on a `None` value
            "writev02\0", // ts
            "writev03\0", // ts
            "writev05\0", // ts
            "writev06\0", // ks
            "writev07\0", // ks
        ],
    );
}

#[allow(unused)]
fn check_ltp() {
    // 对于run_ltp_tests_musl函数，
    // 你可以传递FILELIST的一个子集（或者切片？）给它
    let test = &ltp::FILELIST[..];
    // 7号存在问题
    ltp::check_ltp_tests_musl(
        test,
        &[
            // [100,200)区间
            // cgroup_fj系列需要带参数的脚本入口，直接跑helper会卡死。
            // 需要验证时使用test_cgroup_fj_function_cpuset_via_script。
            "cgroup_regression_3_1.sh\0",         // mkdir: can't create directory '/0': File exists
            "cgroup_regression_3_2.sh\0", // cat: can't open '/proc/sched_debug': No such file or directory
            "cgroup_regression_5_1.sh\0", // 卡死
            "cgroup_regression_5_2.sh\0", // 卡死
            "cgroup_regression_6_1.sh\0", // 卡死
            "cgroup_regression_6_2.sh\0", // 卡死
            "cgroup_regression_fork_processes\0", // 卡死
            "cgroup_regression_getdelays\0", // socket(domain=Netlink)
            "clock_nanosleep01\0",        // 卡死
            "clock_nanosleep04\0",        // 卡死
            "clone02\0",                  // translate_va失败？
            "clone03\0", // [kernel] Panicked at src/mm/memory_set.rs:1241 called `Option::unwrap()` on a `None` value
            "clone08\0", // [kernel] Panicked at src/task/task/process.rs:151 process[4] removed but still refed! refcnt=2
            "connect01\0", // [kernel] Panicked at src/fs/vfs.rs:129 not implemented
            "cpuctl_fj_cpu-hog\0", // 卡死
            // [200,300)区间
            "cpufreq_boost\0",  // panic
            "crash02\0", // [kernel] Panicked at src/mm/translate.rs:106 called `Option::unwrap()` on a `None` value
            "creat06\0", // get_proc_by_hartid: fail because hartid=29 is too large! + error:ext4_fopen:
            "creat07\0", // [kernel] Panicked at src/mm/translate.rs:212 called `Option::unwrap()` on a `None` value
            "cve-2017-17052\0", // [kernel] Panicked at src/mm/memory_set.rs:1241 called `Option::unwrap()` on a `None` value
            "dio_append\0", // [kernel] Panicked at src/mm/memory_set.rs:591 called `Option::unwrap()` on a `None` value
            "dio_read\0", // [kernel] Panicked at src/mm/memory_set.rs:1241 called `Option::unwrap()` on a `None` value
            "dio_sparse\0", // [kernel] Panicked at src/mm/frame_alloc/buddy_cma.rs:63 called `Result::unwrap()` on an `Err` value: ()
            "dio_truncate\0", // [kernel] Panicked at src/mm/memory_set.rs:591 called `Option::unwrap()` on a `None` value
            "diotest4\0", // [kernel] Panicked at src/trap/mod.rs:170 Unsupported trap Unknown, stval = 0x0!
            "diotest6\0", // 卡死
            "dirty\0",    // 时间较长 + warn
            "dirtyc0w\0", // 时间较长 + error
            "dirtyc0w_shmem\0", // 卡死 + error：Unsupported syscall_id: 144, kernel exit this process with exitcode=-1!
            "dirtypipe\0", // [kernel] Panicked at src/syscall/fs.rs:783 called `Option::unwrap()` on a `None` value
            "doio\0",      // 卡死
            // [300,400)区间
            "epoll_wait05\0", // [kernel] Panicked at src/mm/memory_set.rs:1241 called `Option::unwrap()` on a `None` value
            "execve02\0", // [kernel] Panicked at src/mm/translate.rs:212 called `Option::unwrap()` on a `None` value
            "execve04\0", // [kernel] Panicked at src/mm/translate.rs:212 called `Option::unwrap()` on a `None` value
            "execve05\0", // [kernel] Panicked at src/mm/translate.rs:212 called `Option::unwrap()` on a `None` value
            "execveat01\0", // [kernel] Panicked at src/mm/translate.rs:212 called `Option::unwrap()` on a `None` value
            "execveat02\0", // [kernel] Panicked at src/mm/translate.rs:212 called `Option::unwrap()` on a `None` value
            "exit_group01\0", // [kernel] Panicked at src/mm/memory_set.rs:1241 called `Option::unwrap()` on a `None` value
            "fanotify12\0", // [kernel] Panicked at src/mm/translate.rs:212 called `Option::unwrap()` on a `None` value
            // [400,500)区间
            "fcntl13\0", // [kernel] Panicked at src/syscall/fs.rs:783 called `Option::unwrap()` on a `None` value
            "fcntl13_64\0", // [kernel] Panicked at src/syscall/fs.rs:783 called `Option::unwrap()` on a `None` value
            "fcntl14\0",    // 时间较长 + error
            "fcntl14_64\0", // 时间较长 + error
            "fcntl34\0", //[kernel] Panicked at src/syscall/fs.rs:783 called `Option::unwrap()` on a `None` value
            "fcntl34_64\0", //[kernel] Panicked at src/syscall/fs.rs:783 called `Option::unwrap()` on a `None` value
            "fcntl35\0", //[kernel] Panicked at src/syscall/fs.rs:783 called `Option::unwrap()` on a `None` value
            "fcntl36\0", // 同上
            "fcntl36_64\0", // 同上
            "fcntl37\0", // 同上
            "fcntl37_64\0", // 同上
            // [500,600)区间
            "flock03\0", // [kernel] Panicked at src/task/task/process.rs:151 process[3] removed but still refed! refcnt=2
            "force_erase.sh\0", // 需输入y/n
            "fork04\0",  // 卡死
            "fork07\0",  // 卡死
            "fork14\0", // 卡死 warn：Seek beyond the end of the file,path is /tmp/LTP_forGbdpEK/ltp_fork14_2,offset is 16228352 while size is 929792
            "fork_exec_loop\0", //[kernel] Panicked at /home/tatlin-os/lwext4_rust/src/ulibc.rs:92 malloc failed
            // [600,700)区间
            "fs_racer_dir_test.sh\0",  // 卡死
            "fs_racer_file_list.sh\0", // 卡死
            "fstat02\0", // [kernel] Panicked at src/syscall/fs.rs:441 not yet implemented
            "fstat02_64\0", // [kernel] Panicked at src/syscall/fs.rs:441 not yet implemented
            "fstat03_64\0", // [kernel] Panicked at src/fs/fstruct.rs:163 index out of bounds: the len is 5 but the index is 100
            "fstatat01\0", // [kernel] Panicked at src/fs/fstruct.rs:163 index out of bounds: the len is 5 but the index is 100
            // [700,800)区间
            "futex_cmp_requeue01\0", // [kernel] Panicked at src/task/futex.rs:280 not implemented
            "futex_cmp_requeue02\0", // [kernel] Panicked at src/task/futex.rs:280 not implemented
            "futex_wait02\0", // [kernel] Panicked at src/mm/memory_set.rs:1241 called `Option::unwrap()` on a `None` value
            "futex_wait04\0", // [kernel] Panicked at src/task/processor.rs:56 get_proc_by_hartid: fail because hartid=18446743800982615104 is too large!
            "futex_wake03\0", // 卡死
            "genfrexp\0",     // 卡死
            "genhypot\0",     // 卡死
            "genmodf\0",      // 卡死
            // [800,900)区间
            "getpid02\0", // [kernel] Panicked at src/mm/memory_set.rs:1241 called `Option::unwrap()` on a `None` value
            "getrusage03\0", // [kernel] Panicked at src/mm/translate.rs:212 called `Option::unwrap()` on a `None` value
            "getrusage04\0", // [kernel] Panicked at src/mm/translate.rs:212 called `Option::unwrap()` on a `None` value
            "getsockopt02\0", // [kernel] Panicked at src/task/task/process.rs:151 process[3] removed but still refed! refcnt=2
            "growfiles\0", // [kernel] Panicked at src/task/task/process.rs:151 process[2] removed but still refed! refcnt=2
            "hackbench\0", // [kernel] Panicked at src/task/task/process.rs:140 called `Option::unwrap()` on a `None` value
            // [900,1000)区间
            "in6_02\0", // [kernel] Panicked at src/fs/files/stdio.rs:90 called `Result::unwrap()` on an `Err` value: Utf8Error { valid_up_to: 59, error_len: Some(1) }
            "inode01\0", // 卡死
            "inode02\0", // 卡死
            // [1000,---)区间
            "ioctl_ns05\0", // [kernel] Panicked at src/task/task/process.rs:151 process[3] removed but still refed! refcnt=2
            "ioctl_ns06\0", // [kernel] Panicked at src/task/task/process.rs:151 process[3] removed but still refed! refcnt=2
            "kill02\0",     // 卡死
            "kill05\0", // [kernel] Panicked at src/task/task/process.rs:151 process[4] removed but still refed! refcnt=2
            "kill06\0", // [kernel] Panicked at src/task/task/process.rs:151 process[4] removed but still refed! refcnt=6
            "kill08\0", // 同上
            "kill09\0", // 卡死
            "kill10\0", // 卡死
            "leapsec01\0", // 卡死
            // [1100,---)区间
            "link02\0", // [kernel] Panicked at src/syscall/fs.rs:441 not yet implemented
            "link04\0", // 同上
            "link05\0", // 同上
            "link08\0", // 同上
            "madvise05\0", // [kernel] Panicked at src/syscall/memory.rs:34 sys_mmap: Failed to convert flags to MmapFlags bitmap: value is 0x8022
            "mallocstress\0", // [kernel] Panicked at src/utils/simple_range.rs:22 start VPN:0xfffffff87bac0 > end VPN:0x4e5921!
            // [1200,---)区间
            "memcg_test_2\0",      // 卡死
            "memcg_test_4\0",      // 卡死
            "memcg_test_4.sh\0",   // 卡死
            "mlockall03\0",        // 卡死
            "mmap-corruption01\0", // [kernel] Panicked at src/mm/frame_alloc/buddy_cma.rs:63 called `Result::unwrap()` on an `Err` value: ()
            "mmap001\0",           // 卡死
            "mmap01\0", // [kernel] Panicked at src/trap/mod.rs:170 Unsupported trap Unknown, stval = 0x2683ffff!
            "mmap12\0", // [kernel] Panicked at src/syscall/memory.rs:34 sys_mmap: Failed to convert flags to MmapFlags bitmap: value is 0x8002
            "mmap15\0", // [kernel] Panicked at src/utils/simple_range.rs:22 start VPN:0xfffffffffffff > end VPN:0x0!
            "mmap17\0", // [kernel] Panicked at src/syscall/memory.rs:34 sys_mmap: Failed to convert flags to MmapFlags bitmap: value is 0x100002
            "mmap18\0", // [kernel] Panicked at src/syscall/memory.rs:34 sys_mmap: Failed to convert flags to MmapFlags bitmap: value is 0x132
            "mmap20\0", // [kernel] Panicked at src/syscall/memory.rs:34 sys_mmap: Failed to convert flags to MmapFlags bitmap: value is 0x403
            "mmapstress01\0", // 卡死
            // [1300,---)区间
            "mprotect02\0",  // ks
            "mprotect03\0",  // ks
            "mprotect04\0",  // ks
            "mremap01\0",    // ks
            "mremap02\0", // [kernel] Panicked at src/syscall/memory.rs:138 called `Option::unwrap()` on a `None` value
            "mremap03\0", // [kernel] Panicked at src/syscall/memory.rs:138 called `Option::unwrap()` on a `None` value
            "mremap04\0", // ts
            "mremap05\0", // [kernel] Panicked at src/syscall/memory.rs:127 fixed && !may_mov
            "mremap06\0", // [kernel] Panicked at src/syscall/memory.rs:138 called `Option::unwrap()` on a `None` value
            "msync02\0", // [kernel] Panicked at src/syscall/memory.rs:34 sys_mmap: Failed to convert flags to MmapFlags bitmap: value is 0x2001
            "msync03\0", // ts
            "mtest01\0", // [kernel] Panicked at src/mm/frame_alloc/buddy_cma.rs:63 called `Result::unwrap()` on an `Err` value: ()
            "munlock02\0", // ks
            "munmap02\0", // ks
            "munmap03\0", // [kernel] Panicked at src/mm/address.rs:249 assertion `left == right` failed
            "nanosleep04\0", // ks
            // [1400,---)区间
            "netstress\0", // 需要使用的和网络相关内容太多
            "nice05\0",    // ks
            "nptl01\0",    // 耗时较长 但success ---------------------
            "open11\0",    // [kernel] Panicked at src/syscall/fs.rs:441 not yet implemented
            "openat01\0", // [kernel] Panicked at src/fs/fstruct.rs:163 index out of bounds: the len is 6 but the index is 100
            "openfile\0", // ks
            "page01\0", // [kernel] Panicked at src/task/task/process.rs:140 called `Option::unwrap()` on a `None` value
            "pause01\0", // 耗时较长 + error
            "pause02\0", // process[2] removed but still refed! refcnt=2
            // [1500,---)区间
            "pidns32\0", // [kernel] Panicked at src/mm/memory_set.rs:1241 called `Option::unwrap()` on a `None` value
            "pipe11\0",  // ks
            "pipe12\0", // [kernel] Panicked at src/syscall/fs.rs:783 called `Option::unwrap()` on a `None` value
            "pipe15\0", // [kernel] Panicked at src/syscall/fs.rs:783 called `Option::unwrap()` on a `None` value
            "pipe2_02\0", // [kernel] Panicked at src/mm/translate.rs:212 called `Option::unwrap()` on a `None` value
            "poll01\0", // [kernel] Panicked at src/mm/translate.rs:145 called `Option::unwrap()` on a `None` value
            "ppoll01\0", // [kernel] Panicked at src/mm/translate.rs:145 called `Option::unwrap()` on a `None` value
            // [1600,---)区间
            "prot_hsymlinks\0", // [kernel] Panicked at src/mm/translate.rs:212 called `Option::unwrap()` on a `None` value
            "pselect02\0",      // ks
            "pselect02_64\0",   // ks
            "pthcli\0",         // ks
            "pthserv\0",        // ks
            "readlinkat02\0", // [kernel] Panicked at src/fs/fstruct.rs:163 index out of bounds: the len is 5 but the index is 18446744073709551615
            "readv02\0",      // ks
            "recv01\0",       // [kernel] Panicked at src/fs/vfs.rs:129 not implemented
            "recvfrom01\0",   // ts
            "recvmsg01\0", // [kernel] Panicked at src/task/task/process.rs:151 process[3] removed but still refed! refcnt=2
            "recvmsg03\0", // [kernel] Panicked at src/task/task/process.rs:151 process[3] removed but still refed! refcnt=2
            // [1700,---)区间
            "rmdir02\0",              // ks
            "rt_sigaction02\0", // [kernel] Panicked at src/mm/translate.rs:212 called `Option::unwrap()` on a `None` value
            "rt_sigprocmask02\0", // ts
            "rt_sigqueueinfo01\0", // ks
            "rt_sigsuspend01\0", // ks
            "run_sched_cliserv.sh\0", // [kernel] Panicked at src/task/task/process.rs:151 process[2] removed but still refed! refcnt=2
            "sched_driver\0", // [kernel] Panicked at src/mm/translate.rs:212 called `Option::unwrap()` on a `None` value
            "sched_getaffinity01\0", // ks
            "sched_getattr01\0", // ks
            // [1800,---)区间
            "select03\0", // [kernel] Panicked at src/task/processor.rs:56 get_proc_by_hartid: fail because hartid=18446743800981759942 is too large!
            "select04\0", // ks
            "semtest_2ns\0", // [kernel] Panicked at src/task/task/process.rs:151 process[5] removed but still refed! refcnt=2
            "send01\0", // [kernel] Panicked at src/task/task/process.rs:151 process[2] removed but still refed! refcnt=2
            "sendfile04\0", // ks
            "sendfile04_64\0", // ks
            "sendmsg01\0", // [kernel] Panicked at src/trap/mod.rs:170 Unsupported trap Unknown, stval = 0x9000!
            "sendto01\0", // [kernel] Panicked at src/task/task/process.rs:151 process[2] removed but still refed! refcnt=2
            "setfsgid03\0", // ks
            "setfsgid03_16\0", // ks
            "setitimer01\0", // [kernel] Panicked at src/syscall/time.rs:47 only support Itimer Real
            "setitimer02\0", // [kernel] Panicked at src/mm/translate.rs:212 called `Option::unwrap()` on a `None` value
            // [1900,---)区间
            "setpgid03\0", // [kernel] Panicked at src/task/futex.rs:204 called `Option::unwrap()` on a `None` value
            "setpriority01\0", // [kernel] Panicked at src/task/task/process.rs:151 process[3] removed but still refed! refcnt=2
            "setrlimit05\0", // [kernel] Panicked at src/task/processor.rs:56 get_proc_by_hartid: fail because hartid=18446744073709551615 is too large!
            "setrlimit06\0", // ks
            "shm_test\0",    // ks
            "shmat03\0", // [kernel] Panicked at src/syscall/memory.rs:251 called `Option::unwrap()` on a `None` value
            "shmat04\0", // [kernel] Panicked at src/syscall/memory.rs:275 [sys_shmctl] unsupport cmd
            "shmctl01\0", // [kernel] Panicked at src/syscall/memory.rs:275 [sys_shmctl] unsupport cmd  +  clockid == 5
            "shmctl03\0", // [kernel] Panicked at src/syscall/memory.rs:275 [sys_shmctl] unsupport cmd
            "shmctl04\0", // ts
            "shmctl06\0", // ts
            "shmctl07\0", // ts
            "shmctl08\0", // ts
            // [2000,---)区间
            "shmt04\0", // [kernel] Panicked at src/mm/memory_set.rs:483 [shm_attach] unimplement attach addr
            "shmt05\0", // [kernel] Panicked at src/mm/memory_set.rs:483 [shm_attach] unimplement attach addr
            "shmt10\0", // [kernel] Panicked at src/task/task/process.rs:151 process[2] removed but still refed! refcnt=2
            "sighold02\0", // ks
            "sigrelse01\0", // [kernel] Panicked at src/task/task/process.rs:151 process[2] removed but still refed! refcnt=2
            "sigsuspend01\0", // ks
            "splice02\0", // [kernel] Panicked at src/syscall/fs.rs:783 called `Option::unwrap()` on a `None` value
            "starvation\0", // ks
            "stat03\0",   // ks
            "stat03_64\0", // ks
            // [2100,---)区间
            "symlink03\0", // [kernel] Panicked at src/mm/translate.rs:106 called `Option::unwrap()` on a `None` value
            "sysctl03\0", // [kernel] Panicked at src/task/task/process.rs:151 process[3] removed but still refed! refcnt=2
            "sysinfo01\0", // [kernel] Panicked at src/mm/translate.rs:212 called `Option::unwrap()` on a `None` value
            "sysinfo02\0", // ts
            // [2200,---)区间
            // -- 无，网络部分
            // [2300,---)区间
            // -- 无，网络部分
            // [2400,---)区间
            // -- 无，网络部分
            // [2500,---)区间
            "tgkill01\0",       // ks
            "tgkill02\0", // [kernel] Panicked at src/signal/signal.rs:125 called `Option::unwrap()` on a `None` value
            "tgkill03\0", // ts
            "thp01\0", // [kernel] Panicked at src/mm/memory_set.rs:1241 called `Option::unwrap()` on a `None` value
            "timed_forkbomb\0", // ks
            "times03\0", // ks
            // [2600,---)区间
            "tst_hexdump\0",      // ks
            "tst_supported_fs\0", // [kernel] Panicked at src/mm/translate.rs:212 called `Option::unwrap()` on a `None` value
            "umask01\0",          // 耗时较长 + error
            "uname02\0",          // ks
            // [2700,---)区间
            "unlink07\0",   // ks
            "unlinkat01\0", // [kernel] Panicked at src/fs/fstruct.rs:163 index out of bounds: the len is 5 but the index is 100
            "utsname01\0", // [kernel] Panicked at src/mm/memory_set.rs:1241 called `Option::unwrap()` on a `None` value
            "utsname02\0", // ts
            "utsname03\0", // ts
            "vma01\0",     // ks
            "vmsplice04\0", // [kernel] Panicked at src/syscall/fs.rs:783 called `Option::unwrap()` on a `None` value
            "waitid01\0", // [kernel] Panicked at src/task/task/process.rs:151 process[3] removed but still refed! refcnt=2
            "waitid04\0", // ts
            "waitid05\0", // ts
            "waitid06\0", // ts
            "waitid07\0", // ts
            "waitid08\0", // ts
            "waitid09\0", // ts
            "waitid11\0", // ts
            "waitpid04\0", // [kernel] Panicked at src/syscall/process.rs:275 [sys_wait4] We cannot handle input.pid<-1 (input.pid=-2147483648)
            "waitpid06\0", // [kernel] Panicked at src/mm/memory_set.rs:1241 called `Option::unwrap()` on a `None` value
            "waitpid07\0", // ks
            "waitpid08\0", // ks
            "waitpid09\0", // ks
            "waitpid10\0", // [kernel] Panicked at src/mm/memory_set.rs:1241 called `Option::unwrap()` on a `None` value
            "waitpid11\0", // ts
            "waitpid12\0", // ks
            "waitpid13\0", // ks
            // [2800,---)区间
            "writev01\0", // [kernel] Panicked at src/syscall/fs.rs:123 called `Option::unwrap()` on a `None` value
            "writev02\0", // ts
            "writev03\0", // ts
            "writev05\0", // ts
            "writev06\0", // ks
            "writev07\0", // ks
        ],
    );
}

#[no_mangle]
#[cfg(target_arch = "riscv64")]
fn main() -> i32 {
    println!("initproc running......");
    // test_socket();
    // 这三个用到了socket
    // run_specific_test("musl\0", "entry-static.exe\0", "socket\0");
    // run_specific_test("musl\0", "entry-static.exe\0", "getpwnam_r_crash\0");
    // run_specific_test("musl\0", "entry-static.exe\0", "getpwnam_r_errno\0");

    test_ltp(); // LTP case单独分组运行，跳过需要单独包装的helper/脚本
    test_cgroup_fj_function_cpuset_via_script(); // cgroup_fj需要带subsystem参数单独测试

    shutdown(); // 似乎有点bug，直接return 0不会导致QEMU退出
    0
}

#[allow(unused)]
fn test_socket() -> i32 {
    println!("---- Test Socket syscall ----");
    // 测试创建 TCP socket
    println!("Testint TCP socket creation...");
    let fd_tcp = socket(AF_INET, SOCK_STREAM, 0);
    if fd_tcp >= 0 {
        println!("SUCCESS: TCP socket created, fd: {}.", fd_tcp);
    } else {
        println!("FAILED: TCP socket creation returned error: {}", fd_tcp);
    }
    // 测试创建 UDP socket
    println!("Testing UDP socket creation...");
    let fd_udp = socket(AF_INET, SOCK_DGRAM, 0);
    if fd_udp >= 0 {
        println!("SUCCESS: UDP socket created, fd: {}.", fd_udp);
    } else {
        println!("FAILED: UDP socket creation returned error: {}.", fd_udp);
    }
    // 测试不支持的协议族
    println!("Testing unsupported domain...");
    let fd_err = socket(1, SOCK_STREAM, 0);
    if fd_err < 0 {
        println!(
            "SUCCESS: Correctly rejected unsupported domain, error: {}.",
            fd_err
        );
    } else {
        println!(
            "FAILED: Should not have created socket for AF_UNIX, but got fd: {}.",
            fd_err
        );
    }
    // 测试无效参数
    let fd_invalid = socket(999, SOCK_STREAM, 0);
    if fd_invalid < 0 {
        println!(
            "SUCCESS: Correctly rejected invalid domain, error: {}.",
            fd_invalid
        );
    }
    0
}
