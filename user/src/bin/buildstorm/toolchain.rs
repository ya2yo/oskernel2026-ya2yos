//! 检查 final-2026 镜像中的 Rust toolchain 是否可用。

use super::common;

const SCRIPT_PATH: &str = "/tmp/buildstorm-toolchain.sh\0";
const SCRIPT_BODY: &str = r#"#!/bin/sh
mount -t proc proc /proc 2>/dev/null
mount -t sysfs sysfs /sys 2>/dev/null
mount -t devtmpfs devtmpfs /dev 2>/dev/null
export PATH=/root/.cargo/bin:/usr/local/bin:/usr/bin:/bin:/sbin:/usr/sbin
export HOME=/root RUSTUP_HOME=/root/.rustup CARGO_HOME=/root/.cargo
export RUSTUP_TOOLCHAIN=nightly-2026-05-28 CARGO_NET_OFFLINE=true

if rustc --version && cargo --version; then
    echo "BUILDSTORM_DEBUG_TOOLCHAIN ok"
    exit 0
fi
echo "BUILDSTORM_DEBUG_TOOLCHAIN fail"
exit 1
"#;

#[allow(dead_code)]
pub fn run() -> i32 {
    common::run_case("toolchain", SCRIPT_PATH, SCRIPT_BODY)
}
