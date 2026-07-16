# LTP unlink07 pathname 错误码修复

## 背景

LTP `unlink07` 覆盖 `unlink(2)` 对不存在文件、空路径、坏用户指针、非目录路径分量和超长 pathname 的错误码。该测试分别在 musl 与 glibc 运行。

## 现象

原始 `log.ans` 中两个运行时均有 4 项通过、2 项失败：

```text
path is empty string expected ENOENT: EISDIR (21)
pathname too long expected ENAMETOOLONG: ENOENT (2)
```

其余不存在路径、坏地址和普通文件作为目录分量的断言均已通过。

## 分析

`unlink07.c` 对空字符串调用 `unlink("")`，并传入一个未以 NUL 终止的 `PATH_MAX + 2` 字节字符串验证过长路径。`read_user_cstr()` 最多读取 `MAX_PATH_LEN`（256）字节；达到上限而没有 NUL 时会返回这 256 字节，因此 syscall 层必须在 inode 查找前把该情况映射为 `ENAMETOOLONG`。

修复前 `sys_unlinkat()` 直接调用 `Process::get_abs_path()`。空相对 pathname 被归一化为当前工作目录，后续删除目录时返回 `EISDIR`。超长 pathname 则进入底层 ext4 查找；lwext4 对这个查找路径返回 `ENOENT`，没有暴露 Linux 所需的 `ENAMETOOLONG`。

## 根因

`sys_unlinkat()` 缺少 pathname 参数层的空值、总长度和单个分量长度校验。相同模块中的 `linkat(2)` 已有这类前置校验，但 `unlinkat(2)` 没有复用。

## 修复

在 `os/src/syscall/fs/ctl.rs` 中：

- 将仅命名为 `check_link_path()` 的私有 helper 泛化为 `check_path_argument()`；它保持 `AT_EMPTY_PATH` 调用方可选择的空路径规则，并可执行 pathname 长度与 255 字节分量长度检查。
- `sys_unlinkat()` 在读取用户 C string 后、调用 `get_abs_path()` 前执行 `check_path_argument(&path, false, true)`。

因此空 pathname 在路径归一化前返回 `ENOENT`，长度达到 256 字节或包含超过 255 字节分量的 pathname 返回 `ENAMETOOLONG`。`AT_REMOVEDIR` flags 和正常 unlink 的 inode 操作路径没有变化。

## 涉及文件

- `os/src/syscall/fs/ctl.rs`

## 验证

执行：

```text
cargo fmt --manifest-path os/Cargo.toml -- --check
make
timeout 120s make TARGET_ARCH=loongarch64 run > /tmp/unlink07-after-fix-loongarch64.log 2>&1
```

- 格式检查通过。
- 根目录 `make` 完成 RISC-V 与 LoongArch64 构建；仅有既有 vendored `smoltcp` warning。
- LoongArch64 QEMU 中 musl 与 glibc `unlink07` 均为 `passed 6 failed 0 broken 0 skipped 0 warnings 0`，两组返回状态为 0 并正常 `shutdown!`。
- 未运行 RISC-V QEMU 单测；两架构均已完成编译验证。
