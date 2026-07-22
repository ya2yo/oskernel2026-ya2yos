//! 独立验证 Rustc artifact 临时 `.rmeta` 的 rename 发布语义。
//!
//! 该 case 不依赖 Cargo，只要求 `/work` 可写，适合优先排查文件系统 rename/write-back。

use super::common;

const SCRIPT_PATH: &str = "/tmp/buildstorm-rename-publish.sh\0";
const SCRIPT_BODY: &str = r#"#!/bin/sh
mount -t proc proc /proc 2>/dev/null
mount -t sysfs sysfs /sys 2>/dev/null
mount -t devtmpfs devtmpfs /dev 2>/dev/null

RENAME_PROBE=/work/buildstorm-rename-artifact-probe
echo "BUILDSTORM_DEBUG_RENAME_PUBLISH begin"
rm -rf "$RENAME_PROBE"
mkdir -p "$RENAME_PROBE/rmeta" || exit 1
printf 'metadata-payload-123456789\n' > "$RENAME_PROBE/rmeta/full.rmeta"
mv "$RENAME_PROBE/rmeta/full.rmeta" "$RENAME_PROBE/final.rmeta"
if [ ! -e "$RENAME_PROBE/rmeta/full.rmeta" ] \
   && [ "$(cat "$RENAME_PROBE/final.rmeta")" = "metadata-payload-123456789" ]; then
    echo "BUILDSTORM_DEBUG_RENAME_PUBLISH ok"
    exit 0
else
    echo "BUILDSTORM_DEBUG_RENAME_PUBLISH fail"
fi
exit 1
"#;

#[allow(dead_code)]
pub fn run() -> i32 {
    common::run_case("rename-publish", SCRIPT_PATH, SCRIPT_BODY)
}
