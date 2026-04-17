use std::env;

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
}
