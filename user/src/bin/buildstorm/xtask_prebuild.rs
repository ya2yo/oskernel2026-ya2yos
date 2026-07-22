//! 非计时预构建 `tg-xtask`，用于单独观察 Cargo/Rustc 的编译失败。
//!
//! 它会返回真实 Cargo 状态；这比正式评分脚本中故意忽略该阶段失败的行为更适合诊断。

use super::common;

const SCRIPT_PATH: &str = "/tmp/buildstorm-xtask-prebuild.sh\0";
const SCRIPT_BODY: &str = r#"#!/bin/sh
mount -t proc proc /proc 2>/dev/null
mount -t sysfs sysfs /sys 2>/dev/null
mount -t devtmpfs devtmpfs /dev 2>/dev/null
export PATH=/root/.cargo/bin:/usr/local/bin:/usr/bin:/bin:/sbin:/usr/sbin
export HOME=/root RUSTUP_HOME=/root/.rustup CARGO_HOME=/root/.cargo
export RUSTUP_TOOLCHAIN=nightly-2026-05-28 CARGO_NET_OFFLINE=true

cd /work/tgoskits 2>/dev/null || {
    echo "BUILDSTORM_DEBUG_XTASK_PREBUILD fail stage=chdir"
    exit 1
}
echo "----- pre-build tg-xtask (untimed) -----"
cargo build -p tg-xtask 2>&1
RC=$?
if [ "$RC" -eq 0 ]; then
    echo "BUILDSTORM_DEBUG_XTASK_PREBUILD ok"
    exit 0
fi
echo "BUILDSTORM_DEBUG_XTASK_PREBUILD fail rc=$RC"
exit "$RC"
"#;

#[allow(dead_code)]
pub fn run() -> i32 {
    common::run_case("xtask-prebuild", SCRIPT_PATH, SCRIPT_BODY)
}
