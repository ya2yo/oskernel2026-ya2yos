//! 删除旧的 MINIBUILD 目录，并用 `cargo new` 创建干净项目。
//!
//! `minibuild_build` 或 `run_minibuild_fresh` 使用它建立 fresh 编译前置条件。

use super::common;

const SCRIPT_PATH: &str = "/tmp/buildstorm-minibuild-prepare.sh\0";
const SCRIPT_BODY: &str = r#"#!/bin/sh
mount -t proc proc /proc 2>/dev/null
mount -t sysfs sysfs /sys 2>/dev/null
mount -t devtmpfs devtmpfs /dev 2>/dev/null
export PATH=/root/.cargo/bin:/usr/local/bin:/usr/bin:/bin:/sbin:/usr/sbin
export HOME=/root RUSTUP_HOME=/root/.rustup CARGO_HOME=/root/.cargo
export RUSTUP_TOOLCHAIN=nightly-2026-05-28 CARGO_NET_OFFLINE=true

echo "BUILDSTORM_DEBUG_MINIBUILD_PREPARE begin"
rm -rf /tmp/minibuild
if cargo new --vcs none /tmp/minibuild >/dev/null 2>&1; then
    echo "BUILDSTORM_DEBUG_MINIBUILD_PREPARE ok"
    exit 0
fi
echo "BUILDSTORM_DEBUG_MINIBUILD_PREPARE fail"
exit 1
"#;

#[allow(dead_code)]
pub fn run() -> i32 {
    common::run_case("minibuild-prepare", SCRIPT_PATH, SCRIPT_BODY)
}
