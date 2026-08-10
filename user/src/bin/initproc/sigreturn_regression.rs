//! BuildStorm 回归测例：编译 `arceos-helloworld`。
//!
//! 该测例在 initproc 的子进程中执行完整的 ArceOS helloworld 编译，验证
//! BuildStorm 中的工具链、目标架构和构建产物均可用。

use user_lib::{execve, exit, fork, println, waitpid};

const ARCEOS_HELLOWORLD_COMPILE: &str = r#"export PATH=/root/.cargo/bin:/usr/local/bin:/usr/bin:/bin:/sbin:/usr/sbin
export HOME=/root RUSTUP_HOME=/root/.rustup CARGO_HOME=/root/.cargo
export RUSTUP_TOOLCHAIN=nightly-2026-05-28 CARGO_NET_OFFLINE=true
export RUST_MIN_STACK=33554432
cd /work/tgoskits || exit 1
case "$(uname -m 2>/dev/null)" in
  loongarch64) AXARCH=loongarch64 ;;
  riscv64) AXARCH=riscv64 ;;
  *) AXARCH=riscv64 ;;
esac
cargo xtask arceos build -p arceos-helloworld --arch "$AXARCH"
"#;

fn expect(condition: bool, message: &str) -> bool {
    if !condition {
        println!("sigreturn regression failed: {}", message);
    }
    condition
}

fn run_case(name: &str, child: fn() -> i32) -> bool {
    let pid = fork();
    if pid < 0 {
        return expect(false, "fork");
    }
    if pid == 0 {
        println!("sigreturn regression: {} triggering", name);
        let code = child();
        println!("sigreturn regression: {} child returned {}", name, code);
        exit(code);
    }
    let mut status = 0;
    let reaped = waitpid(pid as usize, &mut status) == pid;
    if !reaped {
        return expect(false, "waitpid");
    }
    println!("sigreturn regression: {} reaped status={}", name, status);
    status == 0
}

/// 编译当前架构对应的 `arceos-helloworld`。
fn case_arceos_helloworld_compile() -> i32 {
    let args = ["/bin/bash\0", "-c\0", ARCEOS_HELLOWORLD_COMPILE];
    let ret = execve(&args);
    println!(
        "sigreturn regression: arceos-helloworld execve failed {}",
        ret
    );
    1
}

pub fn run() -> bool {
    let cases: [(&str, fn() -> i32); 1] =
        [("arceos-helloworld-compile", case_arceos_helloworld_compile)];
    let mut ok = true;
    for (name, child) in cases {
        ok = run_case(name, child) && ok;
    }
    println!("sigreturn regression: {}", if ok { "PASS" } else { "FAIL" });
    ok
}
