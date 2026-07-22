//! 运行完整且计时的 `cargo xtask arceos build` 诊断。
//!
//! 这是耗时最长的 case；单独运行可能复用缓存，不能代表正式 cold-build 性能结果。

use super::common;

const SCRIPT_PATH: &str = "/tmp/buildstorm-xtask-build.sh\0";
const SCRIPT_BODY: &str = r#"#!/bin/sh
mount -t proc proc /proc 2>/dev/null
mount -t sysfs sysfs /sys 2>/dev/null
mount -t devtmpfs devtmpfs /dev 2>/dev/null
export PATH=/root/.cargo/bin:/usr/local/bin:/usr/bin:/bin:/sbin:/usr/sbin
export HOME=/root RUSTUP_HOME=/root/.rustup CARGO_HOME=/root/.cargo
export RUSTUP_TOOLCHAIN=nightly-2026-05-28 CARGO_NET_OFFLINE=true

case "$(uname -m 2>/dev/null)" in
  loongarch64) AXARCH=loongarch64; AXTGT=loongarch64-unknown-linux-musl ;;
  riscv64)     AXARCH=riscv64;     AXTGT=riscv64gc-unknown-linux-musl ;;
  *)           AXARCH=riscv64;     AXTGT=riscv64gc-unknown-linux-musl ;;
esac
cd /work/tgoskits 2>/dev/null || exit 1
echo "----- build arceos-helloworld (timed, arch=$AXARCH) -----"
echo "BUILDSTORM_DEBUG_BEGIN mode=multi"
T0=$(cut -d' ' -f1 /proc/uptime 2>/dev/null)
{ timeout 14400 cargo xtask arceos build -p arceos-helloworld --arch "$AXARCH" 2>&1; echo $? > /work/.build.rc; } | tee /work/buildstorm.build.out
RC=$(cat /work/.build.rc 2>/dev/null || echo 1); rm -f /work/.build.rc
T1=$(cut -d' ' -f1 /proc/uptime 2>/dev/null)
ELAPSED=$(awk "BEGIN{printf \"%.2f\", (\"$T1\"+0)-(\"$T0\"+0)}" 2>/dev/null); [ -z "$ELAPSED" ] && ELAPSED=0
ART=$(find target -type f \( -name 'arceos-helloworld' -o -name 'helloworld' \) 2>/dev/null | head -1)
BYTES=0
[ -n "$ART" ] && BYTES=$(wc -c <"$ART")
if [ "$RC" -eq 0 ] && [ -n "$ART" ] && [ "$BYTES" -ge 500000 ]; then
    echo "BUILDSTORM_DEBUG_COMPILE mode=multi ok=true elapsed_s=$ELAPSED cores=$(nproc) bytes=$BYTES arch=$AXARCH"
    exit 0
fi
echo "BUILDSTORM_DEBUG_COMPILE mode=multi ok=false rc=$RC elapsed_s=$ELAPSED cores=$(nproc) bytes=$BYTES arch=$AXARCH"
echo "----- buildstorm.build.out tail -----"
tail -25 /work/buildstorm.build.out 2>/dev/null
exit "$RC"
"#;

#[allow(dead_code)]
pub fn run() -> i32 {
    common::run_case("xtask-build", SCRIPT_PATH, SCRIPT_BODY)
}
