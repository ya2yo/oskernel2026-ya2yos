# ext4 getdents64 d_type ABI 映射错误

## 背景

lwext4 返回的目录项 `inode_type` 使用 ext4 的 `EXT4_DE_*` 枚举；内核随后将
`OsDirent` 原样序列化给 Linux `getdents64(2)` 用户 ABI。两套枚举的数值并不兼容。

## 现象

BuildStorm 初始日志包含：

```text
rm: cannot remove '/tmp/minibuild/src': Is a directory
rm: cannot remove '/tmp/minibuild/target': Is a directory
```

镜像中 `src`、`target` 均为真实 ext4 目录。Cargo 也报告
`lookup_AxVmDevices` bench 不存在，尽管镜像中实际存在目录式布局
`benches/lookup_AxVmDevices/main.rs`。

## 分析

`Ext4File::read_dir_from()` 原来将 `dentry.inode_type` 直接写入 Linux
`dirent64.d_type`。典型冲突如下：

| ext4 类型 | ext4 值 | 用户态按 Linux `DT_*` 解读 | 正确 Linux 值 |
| --- | ---: | --- | ---: |
| `EXT4_DE_REG_FILE` | 1 | `DT_FIFO` | `DT_REG = 8` |
| `EXT4_DE_DIR` | 2 | `DT_CHR` | `DT_DIR = 4` |
| `EXT4_DE_CHRDEV` | 3 | 未定义类型 | `DT_CHR = 2` |
| `EXT4_DE_BLKDEV` | 4 | `DT_DIR` | `DT_BLK = 6` |
| `EXT4_DE_FIFO` | 5 | 未定义类型 | `DT_FIFO = 1` |
| `EXT4_DE_SOCK` | 6 | `DT_BLK` | `DT_SOCK = 12` |
| `EXT4_DE_SYMLINK` | 7 | 未定义类型 | `DT_LNK = 10` |

GNU `rm -rf` 信任非 `DT_UNKNOWN` 的 `d_type`。它把真实目录看成字符设备，因而不递归，
后续普通 `unlinkat(..., 0)` 正确收到 `EISDIR`。Cargo/Rust 的目录扫描同样信任该字段，
从而错过目录式 benchmark。

## 根因

ext4 磁盘目录项类型属于文件系统内部 ABI，Linux `getdents64` 的 `d_type` 属于用户态 ABI。
将前者的数值直接透传跨越了两个不兼容的枚举域。

## 修复

在 `crates/lwext4_rust/src/file.rs` 新增明确的 `EXT4_DE_* -> DT_*` 转换函数：

- regular file、directory、字符设备、块设备、FIFO、socket、symlink 分别映射到 Linux
  定义的 `DT_REG`、`DT_DIR`、`DT_CHR`、`DT_BLK`、`DT_FIFO`、`DT_SOCK`、`DT_LNK`。
- unknown 或未识别值保守返回 `DT_UNKNOWN`，允许用户态回退到 `stat`。
- `read_dir_from()` 仅写入转换后的 `d_type`；目录读取偏移和 `d_reclen` 序列化保持不变。

## 涉及文件

- `crates/lwext4_rust/src/file.rs`

## 验证

执行 `make` 后 RISC-V、LoongArch64 release 构建均通过。RISC-V final-2026 release 回归中：

- `rm: cannot remove ... Is a directory` 不再出现。
- Cargo 对 `lookup_AxVmDevices` 的目录式 bench 缺失报错不再出现。

后续 BuildStorm 在 Rust 工具链与编译阶段继续推进；完整编译受当前 `2G / 2 CPU` QEMU
资源限制，见动态库路径问题复盘中的验证边界。
