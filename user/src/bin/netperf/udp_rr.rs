use crate::{cleanup_testsuit_children, fork_and_run};
use user_lib::println;

const UDP_RR_SCRIPT: &str = concat!(
    "./netserver -D -L 127.0.0.1 -p 12865 & ",
    "server_pid=$!; ",
    "echo '====== netperf UDP_RR begin ======'; ",
    "./netperf -H 127.0.0.1 -p 12865 -t UDP_RR -l 1 -- ",
    "-s 16k -S 16k -m 1k -M 1k -r 64,64 -R 1; ",
    "status=$?; ",
    "kill -9 $server_pid; ",
    "wait $server_pid; ",
    "exit $status\0",
);

/// Run only the UDP request/response case from the upstream netperf script.
///
/// The server lifetime and client arguments intentionally match
/// `netperf_testcode.sh`; keeping both endpoints under the shell preserves the
/// original fork topology while excluding the other netperf workloads.
#[allow(unused)]
pub fn run_udp_rr_musl() -> i32 {
    println!("#### OS COMP TEST GROUP START netperf-udp-rr-musl ####");
    let status = fork_and_run("musl\0", &["busybox\0", "sh\0", "-c\0", UDP_RR_SCRIPT]);
    if status == 0 {
        println!("====== netperf UDP_RR end: success ======");
    } else {
        println!("====== netperf UDP_RR end: fail ({}) ======", status);
    }
    cleanup_testsuit_children();
    println!("#### OS COMP TEST GROUP END netperf-udp-rr-musl ####");
    status
}
