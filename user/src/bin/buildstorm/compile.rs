//! BuildStorm 编译成功评分点。
//!
//! `compile_time::run` 复用同一份冷构建流程，只额外采样 guest 内的耗时，
//! 避免两个评分点的构建语义漂移。

use alloc::format;

use super::common;

const COMPILE_SCRIPT_PATH: &str = "/tmp/buildstorm-compile.sh\0";
const COMPILE_TIME_SCRIPT_PATH: &str = "/tmp/buildstorm-compile-time.sh\0";

const SCRIPT_BODY: &str = r#"mount -t proc proc /proc 2>/dev/null
mount -t sysfs sysfs /sys 2>/dev/null
mount -t devtmpfs devtmpfs /dev 2>/dev/null
export PATH=/root/.cargo/bin:/usr/local/bin:/usr/bin:/bin:/sbin:/usr/sbin
export HOME=/root RUSTUP_HOME=/root/.rustup CARGO_HOME=/root/.cargo
export RUSTUP_TOOLCHAIN=nightly-2026-05-28 CARGO_NET_OFFLINE=true
# Rustc reports stack exhaustion in its parallel compilation workers on this workload.
export RUST_MIN_STACK=33554432

case "$(uname -m 2>/dev/null)" in
  loongarch64) AXARCH=loongarch64; AXTGT=loongarch64-unknown-linux-musl ;;
  riscv64)     AXARCH=riscv64;     AXTGT=riscv64gc-unknown-linux-musl ;;
  *)           AXARCH=riscv64;     AXTGT=riscv64gc-unknown-linux-musl ;;
esac

cd /work/tgoskits 2>/dev/null || {
    if [ "$BUILDSTORM_SCORE_POINT" = "compile-time" ]; then
        echo "BUILDSTORM_DEBUG_COMPILE_TIME ok=false elapsed_s=0 cores=$(nproc) bytes=0 arch=$AXARCH"
    else
        echo "BUILDSTORM_DEBUG_COMPILE ok=false stage=chdir cores=$(nproc) bytes=0 arch=$AXARCH"
    fi
    exit 1
}

rm -rf "target/$AXTGT"
echo "----- pre-build tg-xtask (untimed) -----"
cargo build -p tg-xtask 2>&1 || true

echo "----- build arceos-helloworld (arch=$AXARCH) -----"
if [ "$BUILDSTORM_SCORE_POINT" = "compile-time" ]; then
    T0=$(cut -d' ' -f1 /proc/uptime 2>/dev/null)
fi
{ timeout 14400 cargo xtask arceos build -p arceos-helloworld --arch "$AXARCH" 2>&1; \
  echo $? > /work/.build.rc; } | tee /work/buildstorm.build.out
RC=$(cat /work/.build.rc 2>/dev/null || echo 1)
rm -f /work/.build.rc

if [ "$BUILDSTORM_SCORE_POINT" = "compile-time" ]; then
    T1=$(cut -d' ' -f1 /proc/uptime 2>/dev/null)
    ELAPSED=$(awk "BEGIN{printf \"%.2f\", (\"$T1\"+0)-(\"$T0\"+0)}" 2>/dev/null)
    [ -z "$ELAPSED" ] && ELAPSED=0
fi

ART=$(find target -type f \( -name 'arceos-helloworld' -o -name 'helloworld' \) 2>/dev/null | head -1)
BYTES=0
[ -n "$ART" ] && BYTES=$(wc -c <"$ART")

if [ "$RC" -eq 0 ] && [ -n "$ART" ] && [ "$BYTES" -ge 500000 ]; then
    if [ "$BUILDSTORM_SCORE_POINT" = "compile-time" ]; then
        echo "BUILDSTORM_DEBUG_COMPILE_TIME ok=true elapsed_s=$ELAPSED cores=$(nproc) bytes=$BYTES arch=$AXARCH"
    else
        echo "BUILDSTORM_DEBUG_COMPILE ok=true cores=$(nproc) bytes=$BYTES arch=$AXARCH"
    fi
    exit 0
fi

if [ "$BUILDSTORM_SCORE_POINT" = "compile-time" ]; then
    echo "BUILDSTORM_DEBUG_COMPILE_TIME ok=false rc=$RC elapsed_s=$ELAPSED cores=$(nproc) bytes=$BYTES arch=$AXARCH"
else
    echo "BUILDSTORM_DEBUG_COMPILE ok=false rc=$RC cores=$(nproc) bytes=$BYTES arch=$AXARCH"
fi
echo "----- buildstorm.build.out tail -----"
tail -25 /work/buildstorm.build.out 2>/dev/null
exit 1
"#;

fn run_score_point(name: &str, script_path: &str, score_point: &str) -> i32 {
    let script = format!(
        "#!/bin/sh\nBUILDSTORM_SCORE_POINT={}\n{}",
        score_point, SCRIPT_BODY
    );
    common::run_case(name, script_path, &script)
}

#[allow(dead_code)]
pub fn run() -> i32 {
    run_score_point("compile", COMPILE_SCRIPT_PATH, "compile")
}

pub(crate) fn run_time() -> i32 {
    run_score_point("compile-time", COMPILE_TIME_SCRIPT_PATH, "compile-time")
}
