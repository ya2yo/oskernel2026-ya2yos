//! 清理参考脚本所用架构的 `/work/tgoskits/target/<target>`。
//!
//! 用于在 `xtask_prebuild` 或 `xtask_build` 前排除已有交叉 target 缓存。

use super::common;

const SCRIPT_PATH: &str = "/tmp/buildstorm-xtask-clean-target.sh\0";
const SCRIPT_BODY: &str = r#"#!/bin/sh
mount -t proc proc /proc 2>/dev/null
mount -t sysfs sysfs /sys 2>/dev/null
mount -t devtmpfs devtmpfs /dev 2>/dev/null
export PATH=/root/.cargo/bin:/usr/local/bin:/usr/bin:/bin:/sbin:/usr/sbin
export HOME=/root RUSTUP_HOME=/root/.rustup CARGO_HOME=/root/.cargo
export RUSTUP_TOOLCHAIN=nightly-2026-05-28 CARGO_NET_OFFLINE=true

case "$(uname -m 2>/dev/null)" in
  loongarch64) AXTGT=loongarch64-unknown-linux-musl ;;
  *)           AXTGT=riscv64gc-unknown-linux-musl ;;
esac
cd /work/tgoskits 2>/dev/null || {
    echo "BUILDSTORM_DEBUG_XTASK_CLEAN_TARGET fail stage=chdir"
    exit 1
}
echo "BUILDSTORM_DEBUG_XTASK_CLEAN_TARGET begin target=$AXTGT"
rm -rf "target/$AXTGT"
if [ $? -eq 0 ]; then
    echo "BUILDSTORM_DEBUG_XTASK_CLEAN_TARGET ok target=$AXTGT"
    exit 0
fi
echo "BUILDSTORM_DEBUG_XTASK_CLEAN_TARGET fail target=$AXTGT"
exit 1
"#;

#[allow(dead_code)]
pub fn run() -> i32 {
    common::run_case("xtask-clean-target", SCRIPT_PATH, SCRIPT_BODY)
}
