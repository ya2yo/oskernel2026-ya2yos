# BusyBox hwclock RTC ioctl 与目录 rename 失败

## 背景

根目录原有 `log.ans` 的 BusyBox musl 和 glibc 组均显示三个失败项：
`hwclock` 返回 `RTC_RD_TIME: Not a tty` / `Inappropriate ioctl for device`，
`mv test_dir test` 返回 `can't rename 'test_dir': Is a directory`，随后
`rmdir test` 因目标目录未创建而返回 `ENOENT`。

为缩小范围，`user/src/bin/busybox/mod.rs` 提供独立入口，在每个 libc 根目录
按 `rm -rf test test_dir -> hwclock -> mkdir test_dir -> mv test_dir test -> rmdir test`
顺序执行并打印子进程 wait status。

## 现象

修复前，musl 和 glibc 的 `hwclock`、`mv` 与后续 `rmdir` 都以 wait status
`256` 退出，即 BusyBox applet 的普通退出码 `1`。`rmdir test_dir` 对照调用成功，
说明原 `rmdir test` 不是目录删除实现的独立故障，而是 `mv` 失败的派生结果。

## 分析

BusyBox `mv` 直接调用 `rename(2)`。`sys_renameat2()` 在调用 inode `rename()` 前，
错误地以 `O_RDWR` 打开源路径；VFS 对目录的写意图打开返回 `EISDIR`，因此目录重命名
从未进入 ext4 的 `file_rename()`。

`/dev/rtc`、`/dev/rtc0` 和 `/dev/misc/rtc` 已映射为 `DevRtc`，但 `DevRtc` 没有实现
`File::ioctl()`，调用会落入 trait 默认的 `ENOTTY`。BusyBox 默认 `hwclock` 使用
`RTC_RD_TIME`，故无法读取时间。

## 根因

- `renameat2` 将只读 inode 查找误建模为源文件的读写打开，错误触发目录写打开限制。
- 虚拟 RTC 设备缺少 Linux `RTC_RD_TIME` ioctl 的用户 ABI 实现。

## 修复

- `sys_renameat2()` 改为以 `O_RDONLY` 打开源路径，仅取得 inode 后委托现有 ext4
  rename 路径；目录重命名不再被 VFS 提前拒绝。
- `DevRtc` 增加 `RTC_RD_TIME` 支持，按 `#[repr(C)] struct rtc_time` 的九个 `i32`
  字段向用户态复制数据。时间从内核 `CLOCK_REALTIME` 取得，并转换为 Linux 所需的
  秒、分、时、日、0-based 月、`1900` 偏移年、星期和年内日；未实现 ioctl 保持 `ENOTTY`。

## 涉及文件

- `os/src/syscall/fs/ctl/namespace.rs`
- `os/src/fs/files/devfs.rs`
- `user/src/bin/busybox/mod.rs`
- `user/src/bin/initproc.rs`

## 验证

- `cargo fmt --manifest-path os/Cargo.toml` 与 `cargo fmt --manifest-path user/Cargo.toml`：通过。
- `git diff --check`：通过。
- `make build-arch TARGET_ARCH=riscv64`：通过。
- `make build-arch TARGET_ARCH=loongarch64`：通过。
- `timeout 90s make run > log.ans 2>&1`：RISC-V musl/glibc 的 `hwclock`、`mv test_dir test`
  和 `rmdir test` 均为 `exit_code=0`，最终输出 `shutdown!`，未发现 `panic`、`TFAIL`、
  `TBROK`、`ERROR` 或 `WARN`。

本轮只运行了由原失败项构成的最小 BusyBox 回归，未重新执行完整
`busybox_testcode.sh`，因此不将此结果表述为 BusyBox 全量通过。LoongArch64 完成了
编译验证，未运行本轮行为回归。
