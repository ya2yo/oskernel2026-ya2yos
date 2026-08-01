//! BuildStorm 评分点单项测试入口。
//!
//! 四个入口可在 `initproc` 中按需临时直接调用。每个 case 会先把自己的 Shell
//! 正文写入 `/tmp/buildstorm-*.sh`，然后由 Bash 执行；不需要也不应再向
//! `/glibc` 手工放置调试脚本。
//!
//! | 入口 | 对应评分项 | 前置条件 |
//! | --- | --- | --- |
//! | `toolchain::run` | `rustc`、`cargo` 和 Rustup 环境 | 无 |
//! | `minibuild::run` | 创建、编译并运行 Hello World | 无，会删除旧目录 |
//! | `compile::run` | 冷构建 `arceos-helloworld` 是否成功 | `/work/tgoskits` 与 Cargo 缓存 |
//! | `compile_time::run` | 冷构建的耗时采样 | `/work/tgoskits` 与 Cargo 缓存 |
//!
//! 这些入口只服务于本地逐项诊断，并使用 `BUILDSTORM_DEBUG_*` 标记。平台
//! 提交路径始终由 `initproc::test_final_2026()` 直接通过
//! `run_final_testsuit("glibc\\0", "buildstorm_testcode.sh\\0")` 执行正式脚本。

mod common;

pub mod cache_capacity;
pub mod compile;
pub mod compile_time;
pub mod minibuild;
pub mod toolchain;
