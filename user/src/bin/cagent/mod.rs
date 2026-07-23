//! Guest-side, single-score-point CAgent tests.
//!
//! Each public function mirrors exactly one entry in `scripts/cagent_testcode.sh`.
//! The platform submission runs that canonical script directly; these entries
//! exist only for isolating one score point while keeping agent output on the
//! serial console.

use alloc::format;
use user_lib::{
    chdir, close, execve, exit, fork, kill, openat, println, sleep, waitpid, write, OpenFlags,
};

mod cpu;
mod date;
mod factorial;
mod fs_create;
mod fs_directory;
mod fs_readwrite;
mod fs_search;
mod fs_usage;
mod kernel;
mod network;

const GLIBC_ROOT: &str = "glibc\0";
const DIAGNOSTIC_SCRIPT_PATH: &str = "/tmp/cagent-diagnostic.sh\0";
const SERVER_START_DELAY_MS: usize = 1000;
const SIGTERM: usize = 15;
const AT_FDCWD: isize = -100;
const SCRIPT_MODE: u32 = 0o600;
const EIO: isize = -5;

#[derive(Clone, Copy)]
struct Case {
    name: &'static str,
    prompt: &'static str,
    validation: &'static str,
    timeout_secs: usize,
}

impl Case {
    const fn new(
        name: &'static str,
        prompt: &'static str,
        validation: &'static str,
        timeout_secs: usize,
    ) -> Self {
        Self {
            name,
            prompt,
            validation,
            timeout_secs,
        }
    }
}

pub fn run_factorial() -> i32 {
    run_case(factorial::CASE)
}

pub fn run_date() -> i32 {
    run_case(date::CASE)
}

pub fn run_network() -> i32 {
    run_case(network::CASE)
}

pub fn run_cpu() -> i32 {
    run_case(cpu::CASE)
}

pub fn run_kernel() -> i32 {
    run_case(kernel::CASE)
}

pub fn run_fs_create() -> i32 {
    run_case(fs_create::CASE)
}

pub fn run_fs_readwrite() -> i32 {
    run_case(fs_readwrite::CASE)
}

pub fn run_fs_directory() -> i32 {
    run_case(fs_directory::CASE)
}

pub fn run_fs_search() -> i32 {
    run_case(fs_search::CASE)
}

pub fn run_fs_usage() -> i32 {
    run_case(fs_usage::CASE)
}

fn materialize_script(path: &str, script: &str) -> Result<(), isize> {
    let fd = openat(
        AT_FDCWD,
        path,
        OpenFlags::O_CREATE | OpenFlags::O_WRONLY | OpenFlags::O_TRUNC,
        SCRIPT_MODE,
    );
    if fd < 0 {
        return Err(fd);
    }

    let bytes = script.as_bytes();
    let mut offset = 0;
    let mut result = Ok(());
    while offset < bytes.len() {
        let remaining = &bytes[offset..];
        let written = write(fd as usize, remaining, remaining.len());
        if written <= 0 {
            result = Err(if written == 0 { EIO } else { written });
            break;
        }
        let written = written as usize;
        if written > remaining.len() {
            result = Err(EIO);
            break;
        }
        offset += written;
    }

    let close_status = close(fd as usize);
    match (result, close_status) {
        (Err(err), _) => Err(err),
        (Ok(()), err) if err < 0 => Err(err),
        (Ok(()), _) => Ok(()),
    }
}

/// A separate server is used for every score point so a hung or malformed
/// request cannot contaminate a later manual run.
fn run_case(case: Case) -> i32 {
    println!("===== START cagent {} =====", case.name);
    let server_pid = start_server();
    sleep(SERVER_START_DELAY_MS);

    let status = run_agent(case);
    stop_server(server_pid);

    if status == 0 {
        println!("===== END cagent {} pass =====", case.name);
    } else {
        println!(
            "===== END cagent {} reject status={} =====",
            case.name, status
        );
    }

    status
}

fn start_server() -> isize {
    let pid = fork();
    if pid == 0 {
        let _ = chdir(GLIBC_ROOT);
        let ret = execve(&["./simple_llm_server", "8080"]);
        println!("simple_llm_server execve failed: {}", ret);
        exit(127);
    }
    pid
}

fn stop_server(pid: isize) {
    if pid <= 0 {
        return;
    }

    let _ = kill(pid as usize, SIGTERM);
    let mut exit_code = 0;
    let _ = waitpid(pid as usize, &mut exit_code);
}

fn run_agent(case: Case) -> i32 {
    let output_path = format!("/tmp/cagent_diagnostic_{}", case.name);
    let timeout_secs = format!("{}", case.timeout_secs);
    const RUN_CASE: &str = r#"timeout "$1"s ./agent_lite --workspace . --host 127.0.0.1 --port 8080 "$2" > "$3" 2>&1;
status=$?;
cat "$3";
if [ "$status" -eq 0 ] && eval "$4" < "$3"; then result=pass; ret=0; else result=reject; ret=1; fi;
rm -f "$3";
echo "testcase cagent $5 $result";
exit $ret"#;

    if let Err(err) = materialize_script(DIAGNOSTIC_SCRIPT_PATH, RUN_CASE) {
        println!("cagent-diagnostic materialize fail: {}", err);
        return err as i32;
    }

    crate::fork_and_run(
        GLIBC_ROOT,
        &[
            "/bin/bash",
            DIAGNOSTIC_SCRIPT_PATH,
            timeout_secs.as_str(),
            case.prompt,
            output_path.as_str(),
            case.validation,
            case.name,
        ],
    )
}
