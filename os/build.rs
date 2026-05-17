#![allow(unused)]

use std::env;
use std::string::String;
use std::vec::Vec;

static TARGET_PATH_RISCV64: &str = "../user/target/riscv64gc-unknown-none-elf/release/";
static TARGET_PATH_LOONGARCH64: &str = "../user/target/loongarch64-unknown-none/release/";

// 进行feature互斥检查，如果同时启用了互斥的feature，则退出编译
fn check_feature_exclusivity(feature_groups: &[&[&str]]) {
    for group in feature_groups {
        let mut enabled = vec![];

        for &feature in *group {
            let var_name = format!("CARGO_FEATURE_{}", feature.to_uppercase());
            if env::var(&var_name).is_ok() {
                enabled.push(feature);
            }
        }

        if enabled.len() > 1 {
            panic!("不能同时启用以下互斥 features: {}", enabled.join(", "));
        }
    }
}

fn main() {
    check_feature_exclusivity(&[
        &["loongarch64", "riscv64"],
        &["error", "warn", "info", "debug", "trace"],
    ]);

    println!("cargo:rerun-if-changed=../user/src/");
    cfg_if::cfg_if! {
        if #[cfg(target_arch = "riscv64")] {
            println!("cargo:rerun-if-changed={}", TARGET_PATH_RISCV64);
        } else if #[cfg(target_arch = "loongarch64")] {
            println!("cargo:rerun-if-changed={}", TARGET_PATH_LOONGARCH64);
        }
    }
}
