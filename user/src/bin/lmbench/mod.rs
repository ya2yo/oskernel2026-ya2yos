use crate::*;

#[allow(unused)]
pub fn run_lmbench_tests_musl() {
    println!("#### OS COMP TEST GROUP START lmbench-musl ####");
    println!("latency measurements");
    fork_and_run(
        "/musl\0",
        &["./lmbench_all\0", "lat_syscall\0", "-P\0", "1\0", "null\0"],
    );
    fork_and_run(
        "/musl\0",
        &["./lmbench_all\0", "lat_syscall\0", "-P\0", "1\0", "read\0"],
    );
    fork_and_run(
        "/musl\0",
        &["./lmbench_all\0", "lat_syscall\0", "-P\0", "1\0", "write\0"],
    );
    fork_and_run("/musl\0", &["./busybox\0", "mkdir\0", "-p\0", "/var/tmp\0"]);
    fork_and_run("/musl\0", &["./busybox\0", "touch\0", "/var/tmp/lmbench\0"]);
    fork_and_run(
        "/musl\0",
        &[
            "./lmbench_all\0",
            "lat_syscall\0",
            "-P\0",
            "1\0",
            "stat\0",
            "/var/tmp/lmbench\0",
        ],
    );
    fork_and_run(
        "/musl\0",
        &[
            "./lmbench_all\0",
            "lat_syscall\0",
            "-P\0",
            "1\0",
            "fstat\0",
            "/var/tmp/lmbench\0",
        ],
    );
    fork_and_run(
        "/musl\0",
        &[
            "./lmbench_all\0",
            "lat_syscall\0",
            "-P\0",
            "1\0",
            "open\0",
            "/var/tmp/lmbench\0",
        ],
    );
    fork_and_run(
        "/musl\0",
        &[
            "./lmbench_all\0",
            "lat_select\0",
            "-n\0",
            "100\0",
            "-P\0",
            "1\0",
            "file\0",
        ],
    );
    fork_and_run(
        "/musl\0",
        &["./lmbench_all\0", "lat_sig\0", "-P\0", "1\0", "install\0"],
    );
    fork_and_run(
        "/musl\0",
        &["./lmbench_all\0", "lat_sig\0", "-P\0", "1\0", "catch\0"],
    );
    // println!("prot start");
    // fork_and_run(
    //     "/musl\0",
    //     &[
    //         "./lmbench_all\0",
    //         "lat_sig\0",
    //         "-P\0",
    //         "1\0",
    //         "prot\0",
    //         "lat_sig\0",
    //     ],
    // );
    // println!("prot end");
    // println!("pipe start");
    // fork_and_run("/musl\0", &["./lmbench_all\0", "lat_pipe\0", "-P\0", "1\0"]);
    // println!("pipe end");

    fork_and_run(
        "/musl\0",
        &["./lmbench_all\0", "lat_proc\0", "-P\0", "1\0", "fork\0"],
    );
    fork_and_run(
        "/musl\0",
        &["./lmbench_all\0", "lat_proc\0", "-P\0", "1\0", "exec\0"],
    );

    fork_and_run("/musl\0", &["./busybox\0", "cp\0", "hello\0", "/tmp\0"]);
    fork_and_run(
        "/musl\0",
        &["./lmbench_all\0", "lat_proc\0", "-P\0", "1\0", "shell\0"],
    );
    fork_and_run(
        "/musl\0",
        &[
            "./lmbench_all\0",
            "lmdd\0",
            "label=\"File /var/tmp/XXX write bandwidth:\"\0",
            "of=/var/tmp/XXX\0",
            "move=1m\0",
            "fsync=1\0",
            "print=3\0",
        ],
    );
    fork_and_run(
        "/musl\0",
        &[
            "./lmbench_all\0",
            "lat_pagefault\0",
            "-P\0",
            "1\0",
            "/var/tmp/XXX\0",
        ],
    );
    fork_and_run(
        "/musl\0",
        &[
            "./lmbench_all\0",
            "lat_mmap\0",
            "-P\0",
            "1\0",
            "512k\0",
            "/var/tmp/XXX\0",
        ],
    );

    println!("file system latency");
    fork_and_run("/musl\0", &["./lmbench_all\0", "lat_fs\0", "/var/tmp\0"]);
    println!("Bandwidth measurements");
    // fork_and_run("/musl\0", &["./lmbench_all\0", "bw_pipe\0", "-P\0", "1\0"]); // Pselect6
    fork_and_run(
        "/musl\0",
        &[
            "./lmbench_all\0",
            "bw_file_rd\0",
            "-P\0",
            "1\0",
            "512k\0",
            "io_only\0",
            "/var/tmp/XXX\0",
        ],
    );
    fork_and_run(
        "/musl\0",
        &[
            "./lmbench_all\0",
            "bw_file_rd\0",
            "-P\0",
            "1\0",
            "512k\0",
            "open2close\0",
            "/var/tmp/XXX\0",
        ],
    );
    fork_and_run(
        "/musl\0",
        &[
            "./lmbench_all\0",
            "bw_mmap_rd\0",
            "-P\0",
            "1\0",
            "512k\0",
            "mmap_only\0",
            "/var/tmp/XXX\0",
        ],
    );
    fork_and_run(
        "/musl\0",
        &[
            "./lmbench_all\0",
            "bw_mmap_rd\0",
            "-P\0",
            "1\0",
            "512k\0",
            "open2close\0",
            "/var/tmp/XXX\0",
        ],
    );
    println!("context switch overhead");
    fork_and_run(
        "/musl\0",
        &[
            "./lmbench_all\0",
            "lat_ctx\0",
            "-P\0",
            "1\0",
            "-s\0",
            "32\0",
            "2\0",
            "4\0",
            "8\0",
            "16\0",
            "24\0",
            "32\0",
            "64\0",
            "96\0",
        ],
    );

    println!("#### OS COMP TEST GROUP END lmbench-musl ####");
}

#[allow(unused)]
pub fn run_lmbench_tests_glibc() {
    println!("#### OS COMP TEST GROUP START lmbench-glibc ####");
    println!("latency measurements");
    fork_and_run(
        "/glibc\0",
        &["./lmbench_all\0", "lat_syscall\0", "-P\0", "1\0", "null\0"],
    );
    fork_and_run(
        "/glibc\0",
        &["./lmbench_all\0", "lat_syscall\0", "-P\0", "1\0", "read\0"],
    );
    fork_and_run(
        "/glibc\0",
        &["./lmbench_all\0", "lat_syscall\0", "-P\0", "1\0", "write\0"],
    );
    fork_and_run(
        "/glibc\0",
        &["./busybox\0", "mkdir\0", "-p\0", "/var/tmp\0"],
    );
    fork_and_run(
        "/glibc\0",
        &["./busybox\0", "touch\0", "/var/tmp/lmbench\0"],
    );
    fork_and_run(
        "/glibc\0",
        &[
            "./lmbench_all\0",
            "lat_syscall\0",
            "-P\0",
            "1\0",
            "stat\0",
            "/var/tmp/lmbench\0",
        ],
    );
    fork_and_run(
        "/glibc\0",
        &[
            "./lmbench_all\0",
            "lat_syscall\0",
            "-P\0",
            "1\0",
            "fstat\0",
            "/var/tmp/lmbench\0",
        ],
    );
    fork_and_run(
        "/glibc\0",
        &[
            "./lmbench_all\0",
            "lat_syscall\0",
            "-P\0",
            "1\0",
            "open\0",
            "/var/tmp/lmbench\0",
        ],
    );
    fork_and_run(
        "/glibc\0",
        &[
            "./lmbench_all\0",
            "lat_select\0",
            "-n\0",
            "100\0",
            "-P\0",
            "1\0",
            "file\0",
        ],
    );
    fork_and_run(
        "/glibc\0",
        &["./lmbench_all\0", "lat_sig\0", "-P\0", "1\0", "install\0"],
    );
    fork_and_run(
        "/glibc\0",
        &["./lmbench_all\0", "lat_sig\0", "-P\0", "1\0", "catch\0"],
    );
    // println!("prot start");
    // fork_and_run(
    //     "/glibc\0",
    //     &[
    //         "./lmbench_all\0",
    //         "lat_sig\0",
    //         "-P\0",
    //         "1\0",
    //         "prot\0",
    //         "lat_sig\0",
    //     ],
    // );
    // println!("prot end");
    // println!("pipe start");
    // fork_and_run("/glibc\0", &["./lmbench_all\0", "lat_pipe\0", "-P\0", "1\0"]);
    // println!("pipe end");

    fork_and_run(
        "/glibc\0",
        &["./lmbench_all\0", "lat_proc\0", "-P\0", "1\0", "fork\0"],
    );
    fork_and_run(
        "/glibc\0",
        &["./lmbench_all\0", "lat_proc\0", "-P\0", "1\0", "exec\0"],
    );

    fork_and_run("/glibc\0", &["./busybox\0", "cp\0", "hello\0", "/tmp\0"]);
    fork_and_run(
        "/glibc\0",
        &["./lmbench_all\0", "lat_proc\0", "-P\0", "1\0", "shell\0"],
    );
    fork_and_run(
        "/glibc\0",
        &[
            "./lmbench_all\0",
            "lmdd\0",
            "label=\"File /var/tmp/XXX write bandwidth:\"\0",
            "of=/var/tmp/XXX\0",
            "move=1m\0",
            "fsync=1\0",
            "print=3\0",
        ],
    );
    fork_and_run(
        "/glibc\0",
        &[
            "./lmbench_all\0",
            "lat_pagefault\0",
            "-P\0",
            "1\0",
            "/var/tmp/XXX\0",
        ],
    );
    fork_and_run(
        "/glibc\0",
        &[
            "./lmbench_all\0",
            "lat_mmap\0",
            "-P\0",
            "1\0",
            "512k\0",
            "/var/tmp/XXX\0",
        ],
    );

    println!("file system latency");
    fork_and_run("/glibc\0", &["./lmbench_all\0", "lat_fs\0", "/var/tmp\0"]);
    println!("Bandwidth measurements");
    // fork_and_run("/glibc\0", &["./lmbench_all\0", "bw_pipe\0", "-P\0", "1\0"]); // Pselect6
    fork_and_run(
        "/glibc\0",
        &[
            "./lmbench_all\0",
            "bw_file_rd\0",
            "-P\0",
            "1\0",
            "512k\0",
            "io_only\0",
            "/var/tmp/XXX\0",
        ],
    );
    fork_and_run(
        "/glibc\0",
        &[
            "./lmbench_all\0",
            "bw_file_rd\0",
            "-P\0",
            "1\0",
            "512k\0",
            "open2close\0",
            "/var/tmp/XXX\0",
        ],
    );
    fork_and_run(
        "/glibc\0",
        &[
            "./lmbench_all\0",
            "bw_mmap_rd\0",
            "-P\0",
            "1\0",
            "512k\0",
            "mmap_only\0",
            "/var/tmp/XXX\0",
        ],
    );
    fork_and_run(
        "/glibc\0",
        &[
            "./lmbench_all\0",
            "bw_mmap_rd\0",
            "-P\0",
            "1\0",
            "512k\0",
            "open2close\0",
            "/var/tmp/XXX\0",
        ],
    );
    println!("context switch overhead");
    fork_and_run(
        "/glibc\0",
        &[
            "./lmbench_all\0",
            "lat_ctx\0",
            "-P\0",
            "1\0",
            "-s\0",
            "32\0",
            "2\0",
            "4\0",
            "8\0",
            "16\0",
            "24\0",
            "32\0",
            "64\0",
            "96\0",
        ],
    );

    println!("#### OS COMP TEST GROUP END lmbench-glibc ####");
}
