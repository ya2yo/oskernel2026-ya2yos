//! 编译并运行 `/tmp/minibuild` 的 Hello World。
//!
//! 单独运行会复用已有目录；需要验证 fresh 路径时选择
//! `buildstorm::run_minibuild_fresh`。

use super::common;

const SCRIPT_PATH: &str = "/tmp/buildstorm-minibuild-build.sh\0";
const SCRIPT_BODY: &str = r#"#!/bin/sh
mount -t proc proc /proc 2>/dev/null
mount -t sysfs sysfs /sys 2>/dev/null
mount -t devtmpfs devtmpfs /dev 2>/dev/null
export PATH=/root/.cargo/bin:/usr/local/bin:/usr/bin:/bin:/sbin:/usr/sbin
export HOME=/root RUSTUP_HOME=/root/.rustup CARGO_HOME=/root/.cargo
export RUSTUP_TOOLCHAIN=nightly-2026-05-28 CARGO_NET_OFFLINE=true

echo "BUILDSTORM_DEBUG_MINIBUILD_BUILD begin"
if ( cd /tmp/minibuild && cargo build >/dev/null 2>&1 ) \
   && [ "$(/tmp/minibuild/target/debug/minibuild)" = "Hello, world!" ]; then
    echo "BUILDSTORM_DEBUG_MINIBUILD ok"
    exit 0
fi
echo "BUILDSTORM_DEBUG_MINIBUILD fail"
exit 1
"#;

#[allow(dead_code)]
pub fn run() -> i32 {
    common::run_case("minibuild-build", SCRIPT_PATH, SCRIPT_BODY)
}
