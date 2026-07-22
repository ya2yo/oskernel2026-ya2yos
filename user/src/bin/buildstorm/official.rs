//! BuildStorm 全量正式评分入口。
//!
//! 正式评分必须使用参考脚本的完整语义与 `BUILDSTORM_*` 标记；分阶段诊断
//! case 则继续使用 `BUILDSTORM_DEBUG_*`，避免局部运行被误计为得分。

use super::common;

const SCRIPT_PATH: &str = "/tmp/buildstorm-official.sh\0";
const SCRIPT_BODY: &str = include_str!("../../../../scripts/buildstorm_testcode.sh");

/// 物化并运行与 `scripts/buildstorm_testcode.sh` 完全相同的全量评分脚本。
#[allow(dead_code)]
pub fn run() -> i32 {
    common::run_case("official", SCRIPT_PATH, SCRIPT_BODY)
}
