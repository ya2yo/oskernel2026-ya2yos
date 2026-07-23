//! BuildStorm 分阶段诊断入口。
//!
//! ## 如何运行单项
//!
//! `initproc::test_final_2026()` 启动时只调用 [`run_selected`]。修改
//! [`SELECTED_CASE`] 为下表中的函数，再重新构建并启动 final-2026 镜像即可。
//! 每个 case 会先把自己的 Shell 正文写入 `/tmp/buildstorm-*.sh`，然后由 Bash
//! 执行；不需要也不应再向 `/glibc` 手工放置调试脚本。
//!
//! | 选择器 | 检查内容 | 前置条件 |
//! | --- | --- | --- |
//! | `toolchain::run` | `rustc`、`cargo` 和 Rustup 环境 | 无 |
//! | `minibuild_prepare::run` | 创建干净的 `/tmp/minibuild` | 无，会删除旧目录 |
//! | `minibuild_build::run` | 编译并运行 Hello World | 已运行 prepare |
//! | `xtask_clean_target::run` | 清理交叉 target | `/work/tgoskits` 存在 |
//! | `rename_publish::run` | `.rmeta` 临时文件 rename 发布 | `/work` 可写 |
//! | `xtask_prebuild::run` | 非计时 `cargo build -p tg-xtask` | `/work/tgoskits` 与 Cargo 缓存 |
//! | `xtask_prebuild::run_serial` | 单作业 prebuild 争用诊断 | 与默认 prebuild 相同 |
//! | `unicode_artifact::run` | `unicode_ident` artifact 的 `rustc --extern` | prebuild 已产出 artifact |
//! | `xtask_build::run` | 计时 `cargo xtask arceos build` | target、工具链和较长运行窗口 |
//!
//! 可组合选择 [`run_minibuild_fresh`] 或 [`run_diagnostics`] 排查问题。全量
//! [`run_official_sequence`] 会直接运行参考脚本，输出正式评分标记；它与
//! 分阶段诊断的输出契约不同。

mod common;

pub mod minibuild_build;
pub mod minibuild_prepare;
pub mod official;
pub mod rename_publish;
pub mod toolchain;
pub mod unicode_artifact;
pub mod xtask_build;
pub mod xtask_clean_target;
pub mod xtask_prebuild;

use user_lib::println;

pub type CaseRunner = fn() -> i32;

// 修改这一行选择一次启动运行的 case。例如：
//
// pub const SELECTED_CASE: CaseRunner = toolchain::run;
// pub const SELECTED_CASE: CaseRunner = run_minibuild_fresh;
// pub const SELECTED_CASE: CaseRunner = run_diagnostics;
//
// `minibuild_build::run` 依赖 prepare；希望强制 fresh 路径时应选择
// `run_minibuild_fresh`，不要只选择 `minibuild_build::run`。
pub const SELECTED_CASE: CaseRunner = xtask_prebuild::run_serial;
#[allow(unused)]
pub fn run_selected() -> i32 {
    SELECTED_CASE()
}

#[allow(dead_code)]
/// 先创建干净项目再构建，保证 MINIBUILD 不会复用先前启动遗留的 target。
pub fn run_minibuild_fresh() -> i32 {
    let status = minibuild_prepare::run();
    if status != 0 {
        return status;
    }
    minibuild_build::run()
}

#[allow(dead_code)]
/// 运行完整的正式评分脚本。
///
/// 参考脚本由 `include_str!` 在构建期嵌入，因此 guest 不依赖 `/glibc` 中是否
/// 预安装 `buildstorm_testcode.sh`。不要用分阶段诊断入口替代此入口进行判分。
pub fn run_official_sequence() -> i32 {
    official::run()
}

#[allow(dead_code)]
/// 运行主阶段以及额外的 rename/unicode artifact 回归探针。
pub fn run_diagnostics() -> i32 {
    run_sequence(
        "diagnostics",
        &[
            toolchain::run,
            minibuild_prepare::run,
            minibuild_build::run,
            rename_publish::run,
            xtask_clean_target::run,
            xtask_prebuild::run,
            unicode_artifact::run,
            xtask_build::run,
        ],
    )
}

fn run_sequence(name: &str, cases: &[CaseRunner]) -> i32 {
    println!("#### OS COMP TEST GROUP START buildstorm-{} ####", name);
    let mut result = 0;
    for run_case in cases {
        let status = run_case();
        if result == 0 && status != 0 {
            result = status;
        }
    }
    println!("Summary: buildstorm-{} status={}", name, result);
    println!("#### OS COMP TEST GROUP END buildstorm-{} ####", name);
    result
}
