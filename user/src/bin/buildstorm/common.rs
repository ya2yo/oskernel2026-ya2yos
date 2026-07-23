//! BuildStorm case 的公共物化与执行器。
//!
//! 每个 case 自带 Shell 正文；这里在运行前写到 `/tmp`，再通过既有 Bash
//! runner 执行。脚本不需要可执行位，也不依赖内核在 `/glibc` 注入文件。

use crate::run_final_testsuit;
use user_lib::{close, openat, println, write, OpenFlags};

const GLIBC_ROOT: &str = "glibc\0";
const AT_FDCWD: isize = -100;
const SCRIPT_MODE: u32 = 0o600;
const EIO: isize = -5;

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

/// 物化并运行一个 case，保留 child 的原始 wait status 供调用方报告。
pub(crate) fn run_case(name: &str, script_path: &str, script_body: &str) -> i32 {
    println!("#### OS COMP TEST GROUP START buildstorm-{} ####", name);
    let status = match materialize_script(script_path, script_body) {
        Ok(()) => run_final_testsuit(GLIBC_ROOT, script_path),
        Err(err) => {
            println!(
                "BUILDSTORM_DEBUG_CASE name={} fail stage=materialize rc={}",
                name, err
            );
            err as i32
        }
    };
    if status == 0 {
        println!("BUILDSTORM_DEBUG_CASE name={} ok", name);
    } else {
        println!("BUILDSTORM_DEBUG_CASE name={} fail status={}", name, status);
    }
    println!("#### OS COMP TEST GROUP END buildstorm-{} ####", name);
    status
}
