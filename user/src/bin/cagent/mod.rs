//! Guest-side, individually runnable CAgent diagnostics.
//!
//! Each case mirrors one entry in `scripts/cagent_testcode.sh`.  The source
//! script remains untouched; this module exists so a failed case can be run
//! in isolation while its agent output stays on the serial console.

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
const SCORE_SCRIPT_PATH: &str = "/tmp/cagent-score.sh\0";
const DIAGNOSTIC_SCRIPT_PATH: &str = "/tmp/cagent-diagnostic.sh\0";
const SERVER_START_DELAY_MS: usize = 1000;
const SIGTERM: usize = 15;
const AT_FDCWD: isize = -100;
const SCRIPT_MODE: u32 = 0o600;
const EIO: isize = -5;

#[derive(Clone, Copy)]
pub struct Case {
    name: &'static str,
    prompt: &'static str,
    validation: &'static str,
    timeout_secs: usize,
}

impl Case {
    pub const fn new(
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

pub static ALL_CASES: [&Case; 10] = [
    &factorial::CASE,
    &date::CASE,
    &network::CASE,
    &cpu::CASE,
    &kernel::CASE,
    &fs_create::CASE,
    &fs_readwrite::CASE,
    &fs_directory::CASE,
    &fs_search::CASE,
    &fs_usage::CASE,
];

/// Run the three cases that rejected in the supplied LoongArch CAgent log.
pub fn run_failed_cases() -> usize {
    run_cases(&[&kernel::CASE, &fs_readwrite::CASE, &fs_directory::CASE])
}

/// Run the canonical scoring script without changing the diagnostic cases.
pub fn run_official_script() -> i32 {
    match materialize_script(SCORE_SCRIPT_PATH, CAGENT_TESTCODE) {
        Ok(()) => crate::run_final_testsuit(GLIBC_ROOT, SCORE_SCRIPT_PATH),
        Err(err) => {
            println!("cagent-score materialize fail: {}", err);
            err as i32
        }
    }
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

/// Run arbitrary CAgent cases sequentially.  A separate server is used for
/// every case, so a hung or malformed request cannot contaminate the next one.
pub fn run_cases(cases: &[&Case]) -> usize {
    let mut failures = 0;

    for case in cases {
        if !run_case(**case) {
            failures += 1;
        }
    }

    println!(
        "===== CAgent diagnostics complete: {}/{} passed =====",
        cases.len() - failures,
        cases.len()
    );
    failures
}

fn run_case(case: Case) -> bool {
    println!("===== START cagent {} =====", case.name);
    let server_pid = start_server();
    sleep(SERVER_START_DELAY_MS);

    let status = run_agent(case);
    stop_server(server_pid);

    if status == 0 {
        println!("===== END cagent {} pass =====", case.name);
        true
    } else {
        println!(
            "===== END cagent {} reject status={} =====",
            case.name, status
        );
        false
    }
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

const CAGENT_TESTCODE: &str = include_str!("../../../../scripts/cagent_testcode.sh");
