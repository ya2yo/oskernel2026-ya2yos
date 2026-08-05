# BuildStorm exclusive 创建的 EEXIST 日志误报

## 背景

Cargo/Rustc 会在 `target/debug/deps` 并发创建随机名称的临时目录，并使用
`O_CREAT|O_EXCL` 保证名称唯一。多个构建任务偶然竞争同一名称时，只有获胜者创建目录，
其他任务收到 `EEXIST` 后由用户态重试或继续处理。

## 现象

`server.ans` 在 `pre-build tg-xtask` 的 Cargo `440/446` 阶段出现：

```text
ext4_dir_mk_exclusive_with_metadata: /work/tgoskits/target/debug/deps/rmetakjaBdl, rc = 17
```

随后构建正常输出 `Finished`，没有 `could not compile` 或测试失败。

## 分析

VFS 的 `open_inner()` 在缓存和路径查找未命中后调用
`create_with_metadata()`；lwext4 的 `ext4_generic_open2_with_metadata()` 在最终目录项已由
另一个 task 创建时按 Linux exclusive-create 语义返回 `EEXIST`。该返回值正确传回 Cargo，
但 Rust wrapper 将所有非零返回统一按 `error!` 记录，导致正常并发竞争看起来像内核错误。

## 根因

日志级别没有区分 `O_EXCL` 创建中预期的 `EEXIST` 和真正的文件系统错误。

## 修复

- `file_open_with_metadata()` 和 `dir_mk_exclusive_with_metadata()` 对 `EEXIST` 使用 `debug!`；
- 保留原始 `Err(EEXIST)` 返回，不改变 `open(O_CREAT|O_EXCL)`、`mkdirat` 或 Cargo 的重试语义；
- 其他 errno 继续使用 `error!`，便于发现真实的 EXT4 故障。

## 涉及文件

- `crates/lwext4_rust/src/file.rs`

## 验证

- `cargo fmt --manifest-path crates/lwext4_rust/Cargo.toml`：通过；
- `git diff --check`：通过；
- `server.ans` 原始上下文确认该 `EEXIST` 后 `tg-xtask` 成功完成；
- 完整 QEMU BuildStorm 尚未重跑。
