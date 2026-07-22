# BuildStorm 全量运行误用诊断标记导致评分器 0 分

## 背景

BuildStorm 的用户态分阶段诊断入口位于 `user/src/bin/buildstorm/`。这些
case 被设计为只输出 `BUILDSTORM_DEBUG_*`，以便单项排查不会被正式评分器
误认为完整竞赛结果。

final-2026 的全量入口随后改为调用
`buildstorm::run_official_sequence()`。该函数当时仍只是分阶段诊断组合，
并不具备 `scripts/buildstorm_testcode.sh` 的完整脚本语义和输出契约。

## 现象

`log.ans` 已包含：

```text
BUILDSTORM_DEBUG_TOOLCHAIN ok
BUILDSTORM_DEBUG_MINIBUILD ok
```

但执行：

```bash
cat log.ans | python3 scripts/judge_buildstorm-glibc.py | jq .
```

时，toolchain、minibuild、compile 和时间四项均为 0，且 judge 输出
`compile: ok=missing`。

## 分析

`scripts/judge_buildstorm-glibc.py` 只接受以下正式标记：

| 评分项 | 必需标记 |
| --- | --- |
| toolchain | `BUILDSTORM_TOOLCHAIN ok` |
| minibuild | `BUILDSTORM_MINIBUILD ok` |
| compile | `BUILDSTORM_COMPILE ... ok=true ...` |
| compile time | 同一条 compile 成功标记中的 `elapsed_s` |

`BUILDSTORM_DEBUG_TOOLCHAIN`、`BUILDSTORM_DEBUG_MINIBUILD` 和
`BUILDSTORM_DEBUG_COMPILE` 都不匹配上述协议。因此已有的诊断成功结果对
judge 等同于不存在。

不能只把分阶段 case 的 `DEBUG` 前缀改掉。正式参考脚本在同一个 Bash
进程中依次执行 `rm -rf /tmp/minibuild`、`cargo new` 和 `cargo build`，并且
故意忽略非计时 `tg-xtask` 预构建的失败；分阶段 case 则会在每项结束后清理
子进程，且保留 prebuild 的真实失败状态。两者的目的和失败路径不同。

## 根因

全量入口误将仅用于定位问题的 `run_official_sequence()` 当成正式评分入口。
该诊断组合刻意输出 `BUILDSTORM_DEBUG_*`，导致 judge 无法识别已完成的计分
阶段。

## 修复

新增 `user/src/bin/buildstorm/official.rs`：

- 通过 `include_str!("../../../../scripts/buildstorm_testcode.sh")` 在构建期嵌入
  唯一的正式参考脚本，避免 Rust 内嵌副本与 `scripts/` 中的评分契约漂移。
- 复用 `common::run_case()` 将脚本物化为 `/tmp/buildstorm-official.sh`，再由
  Bash 一次性执行；guest 不再依赖 `/glibc` 是否预装该脚本。
- 将 `run_official_sequence()` 改为只委托这个正式 runner。

原有 `toolchain`、`minibuild_*`、`xtask_*` 和扩展诊断组合保持
`BUILDSTORM_DEBUG_*`，继续用于局部故障定位。未修改 judge 以兼容 DEBUG
标记，因为正式外部评分仍应遵守 canonical `BUILDSTORM_*` 协议。

## 涉及文件

- `user/src/bin/buildstorm/official.rs`
- `user/src/bin/buildstorm/mod.rs`
- `Docs/决赛文档/problem/buildstorm-minibuild-post-toolchain-stall.md`

## 验证

- `git diff --check`：通过。
- `sh -n scripts/buildstorm_testcode.sh`：通过。
- 将三条成功的 canonical 标记输入
  `python3 scripts/judge_buildstorm-glibc.py | jq .`：得到 `180.0 / 180.0`，
  验证正式协议会被当前 judge 正确解析。
- `make`：RISC-V 与 LoongArch64 release 构建均通过。只有既有 Cargo config
  弃用和 smoltcp warnings。
- `timeout 180s make run TARGET_ARCH=riscv64 > /tmp/buildstorm-full-marker-riscv.log 2>&1`：
  实际 guest 日志已输出 `BUILDSTORM_TOOLCHAIN ok` 和
  `BUILDSTORM_MINIBUILD ok`。用 judge 解析该日志得到 `20.0 / 180.0`，证明此前
  丢失的两项正式标记已恢复。

该 QEMU 运行在 `cargo build -p tg-xtask` 预构建期间到达外部 180 秒上限，尚未
出现 `BUILDSTORM_COMPILE`。因此本次不宣称完整 ArceOS 编译、40 分 compile
项或 120 分时间项已通过。
