# LTP mkdir09 目录 mode 类型位掩码错误

## 背景

`mkdir09` 会创建多个线程，在同一个挂载目录下并发执行三类操作：

- 对预先存在的目录重复 `mkdir()`，期望返回 `EEXIST`。
- 对不存在的目录执行 `rmdir()`，期望返回 `ENOENT`。
- 反复创建并删除 `MODE_RWX=07770` 的临时目录，期望 `mkdir()` 和 `rmdir()` 都成功。

该用例覆盖目录创建、删除、并发路径查询以及 `st_mode` 中类型位和权限位的组合语义。

## 现象

`log.ans` 中 `mkdir09` 大量失败：

```text
mkdir09.c:82: TFAIL: mkdir(tmpdir, MODE_RWX) failed: EEXIST (17)
mkdir09.c:88: TFAIL: rmdir(tmpdir) failed: ENOTDIR (20)
```

同时内核日志持续输出：

```text
Unknown inode mode type 4e00
```

最终 LTP 汇总为：

```text
Summary:
passed   6
failed   945
broken   0
skipped  0
warnings 0
FAIL LTP CASE mkdir09 : 10
```

## 分析

`mkdir09.c` 的 `test3()` 使用 `mkdir(tmpdir, MODE_RWX)` 创建目录，其中 `MODE_RWX` 为 `07770`。这不只是普通 `0777` 权限，还包含 setgid/sticky 等特殊权限位。

Ya2yOS 的 `sys_mkdirat()` 通过 `open(abs_path, O_CREATE | O_DIRECTORY, mode)` 创建目录，最终调用 `Ext4Inode::fmode_set(mode)` 写入 ext4 mode。原先 `lwext4_rust::file_type()` 通过：

```rust
let cal: u32 = 0o777;
let types = mode & (!cal);
```

提取类型位。这只清除了普通权限位，没有清除 `0o7000` 特殊权限位。对于目录 `S_IFDIR | 07770`，得到的 `types` 为 `0x4e00`，既不是纯 `0x4000` 目录类型，也不是其他合法类型，于是日志打印 `Unknown inode mode type 4e00` 并退化为普通文件类型。

目录被误判为普通文件后，`unlinkat(..., AT_REMOVEDIR)` 会认为目标不是目录并返回 `ENOTDIR`。删除失败又会留下路径，下一轮 `mkdir()` 命中已有路径并返回 `EEXIST`，于是形成日志中的交替失败。

此外，创建路径原先对新 inode 写 mode 时只传入权限位，若调用方没有带 `S_IF*` 类型位，可能导致 `st_mode` 类型信息不完整。`chmod/fchmodat` 又按 Linux 语义只传权限位，因此底层 `fmode_set()` 需要在调用方未提供类型位时自动补齐当前 inode 类型。

## 根因

`lwext4_rust::file_type()` 提取 inode 类型时掩码错误，只排除了 `0o777` 普通权限位，未排除 `0o7000` 特殊权限位，导致带 setgid/sticky 等特殊权限的目录 mode 被识别为未知类型。

## 修复

修改 `crates/lwext4_rust/src/file.rs`：

- 使用 Linux 标准类型掩码 `0o170000` 提取 `S_IF*` 类型位。
- `S_IFDIR | 07770` 现在会正确得到 `0o040000`，识别为目录。

修改 `os/src/fs/mod.rs` 和 `os/src/fs/ext4_lw/inode.rs`：

- 为 `InodeType` 增加 `mode_bits()`，集中映射 FIFO、字符设备、目录、块设备、普通文件、符号链接、socket 的 `S_IF*` 类型位。
- `Ext4Inode::fmode_set()` 写 mode 时保留调用方显式传入的类型位；若调用方只传权限位，则根据当前 inode 类型补齐类型位，再与 `0o7777` 权限/特殊权限位组合。

这样既修复 `mkdirat()` 创建目录时的完整 `st_mode`，也避免 `chmod/fchmodat` 抹掉文件类型位，同时不破坏 `mknodat()` 传入 FIFO/设备/socket 类型位的路径。

## 涉及文件

- `crates/lwext4_rust/src/file.rs`
- `os/src/fs/mod.rs`
- `os/src/fs/ext4_lw/inode.rs`

## 验证

已执行：

```text
make
```

结果：

- 当前默认 `TARGET_ARCH=loongarch64`，构建通过。
- `log.ans` 作为本次运行结果分析来源，失败信号与上述根因匹配。

未执行：

- 未重新运行 `make run`。此前沙箱内 QEMU 因 `/var/tmp` 只读无法启动，用户随后明确说明 `log.ans` 是运行结果，因此本次只做编译验证和日志分析。
- 未运行 `riscv64`。
