//! BuildStorm MINIBUILD 评分点。
//!
//! 该单项保持正式脚本的完整语义：先重建 `/tmp/minibuild`，再编译并运行
//! Hello World，避免复用前一次启动留下的 target。

use super::common;

const SCRIPT_PATH: &str = "/tmp/buildstorm-minibuild.sh\0";
const SCRIPT_BODY: &str = r#"#!/bin/sh
mount -t proc proc /proc 2>/dev/null
mount -t sysfs sysfs /sys 2>/dev/null
mount -t devtmpfs devtmpfs /dev 2>/dev/null
export PATH=/root/.cargo/bin:/usr/local/bin:/usr/bin:/bin:/sbin:/usr/sbin
export HOME=/root RUSTUP_HOME=/root/.rustup CARGO_HOME=/root/.cargo
export RUSTUP_TOOLCHAIN=nightly-2026-05-28 CARGO_NET_OFFLINE=true

rm -rf /tmp/minibuild
if cargo new --vcs none /tmp/minibuild >/dev/null 2>&1 \
   && ( cd /tmp/minibuild && cargo build >/dev/null 2>&1 ) \
   && [ "$(/tmp/minibuild/target/debug/minibuild)" = "Hello, world!" ]; then
    echo "BUILDSTORM_DEBUG_MINIBUILD ok"
    exit 0
fi
echo "BUILDSTORM_DEBUG_MINIBUILD fail"
exit 1
"#;

#[allow(dead_code)]
pub fn run() -> i32 {
    common::run_case("minibuild", SCRIPT_PATH, SCRIPT_BODY)
}
