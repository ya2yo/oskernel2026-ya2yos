//! 验证预构建生成的 `unicode_ident` artifact 可以被 `rustc --extern` 读取。
//!
//! 先运行 `xtask_prebuild`，否则该 case 会因找不到 artifact 而失败。

use super::common;

const SCRIPT_PATH: &str = "/tmp/buildstorm-unicode-artifact.sh\0";
const SCRIPT_BODY: &str = r#"#!/bin/sh
mount -t proc proc /proc 2>/dev/null
mount -t sysfs sysfs /sys 2>/dev/null
mount -t devtmpfs devtmpfs /dev 2>/dev/null
export PATH=/root/.cargo/bin:/usr/local/bin:/usr/bin:/bin:/sbin:/usr/sbin
export HOME=/root RUSTUP_HOME=/root/.rustup CARGO_HOME=/root/.cargo
export RUSTUP_TOOLCHAIN=nightly-2026-05-28 CARGO_NET_OFFLINE=true

cd /work/tgoskits 2>/dev/null || {
    echo "BUILDSTORM_DEBUG_UNICODE_ARTIFACT fail stage=chdir"
    exit 1
}
CARGO_TARGET_DIR=/work/tgoskits/target
PROBE_SRC=/work/unicode-ident-probe.rs
PROBE_OUT=/work/unicode-ident-probe
printf '%s\n' "extern crate unicode_ident; fn main() { assert!(unicode_ident::is_xid_start('a')); }" > "$PROBE_SRC"
FOUND_ARTIFACT=0
FAILED=0
echo "BUILDSTORM_DEBUG_UNICODE_ARTIFACT begin"
for ARTIFACT in "$CARGO_TARGET_DIR"/debug/deps/libunicode_ident-*.rmeta "$CARGO_TARGET_DIR"/debug/deps/libunicode_ident-*.rlib; do
    [ -f "$ARTIFACT" ] || continue
    FOUND_ARTIFACT=1
    echo "BUILDSTORM_DEBUG_UNICODE_ARTIFACT artifact=$ARTIFACT"
    ls -ln "$ARTIFACT"
    wc -c < "$ARTIFACT"
    if rustc --edition=2021 "$PROBE_SRC" --extern "unicode_ident=$ARTIFACT" -o "$PROBE_OUT"; then
        echo "BUILDSTORM_DEBUG_UNICODE_ARTIFACT probe=ok artifact=$ARTIFACT"
    else
        FAILED=1
        echo "BUILDSTORM_DEBUG_UNICODE_ARTIFACT probe=fail artifact=$ARTIFACT"
    fi
done
[ "$FOUND_ARTIFACT" -eq 1 ] || echo "BUILDSTORM_DEBUG_UNICODE_ARTIFACT artifact=missing"
if [ "$FOUND_ARTIFACT" -eq 1 ] && [ "$FAILED" -eq 0 ]; then
    echo "BUILDSTORM_DEBUG_UNICODE_ARTIFACT ok"
    exit 0
fi
echo "BUILDSTORM_DEBUG_UNICODE_ARTIFACT fail"
exit 1
"#;

#[allow(dead_code)]
pub fn run() -> i32 {
    common::run_case("unicode-artifact", SCRIPT_PATH, SCRIPT_BODY)
}
