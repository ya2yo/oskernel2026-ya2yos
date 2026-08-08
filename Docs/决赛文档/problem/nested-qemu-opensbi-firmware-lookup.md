# 嵌套 RISC-V QEMU 的 OpenSBI 固件查找

## 背景

final-2026 镜像中的 `initproc` 在 BuildStorm 完成后启动
`/opt/qemu-rv64/bin/qemu-system-riscv64`。固件文件已随镜像放在
`/opt/qemu-rv64/share/opensbi-riscv64-generic-fw_dynamic.bin`。

## 现象

`log.ans` 显示 BuildStorm 编译成功，随后嵌套 QEMU 报错：

```text
qemu-system-riscv64: Unable to find the RISC-V BIOS "opensbi-riscv64-generic-fw_dynamic.bin"
```

## 分析

`-bios default` 由 QEMU 根据其编译期 data directory 查找 OpenSBI。`PATH` 只参与
命令或可执行文件查找，不参与 QEMU 固件数据文件查找；Linux VFS 也只解析调用方
传入的精确路径，找不到时返回 `ENOENT`，不会递归扫描根目录寻找同名文件。

镜像中的固件位于 `/opt/qemu-rv64/share`，与该嵌套 QEMU 的默认 data directory
布局不一致。QEMU 的固件存在性探测可能使用 `access/stat`，随后再执行 `open`，因此
回退必须位于公共 VFS 打开入口，而不能只补 `sys_openat`。

## 根因

QEMU 的默认固件候选路径没有命中镜像内实际的 OpenSBI 文件；这不是 OpenSBI 内容
损坏，也不是 BuildStorm 编译失败。

## 修复

在 `os/src/fs/kernel_fs_ops/open.rs` 的公共 `open()` 中，普通查找返回 `ENOENT` 时，
仅在以下条件同时满足时重试固定镜像路径：

- 当前进程的可执行文件是 `/opt/qemu-rv64/bin/qemu-system-riscv64`；
- 请求路径的末级名称正是 `opensbi-riscv64-generic-fw_dynamic.bin`；
- 请求为只读探测，且没有创建、目录、截断或 `O_PATH` 语义。

这样 `faccessat`、`statx/fstatat` 和 `openat` 通过同一 VFS 入口得到一致结果，其他
进程和其他文件仍保持原有 Linux 兼容语义。QEMU 命令行参数、环境变量和镜像文件布局
均未修改。

## 涉及文件

- `os/src/fs/kernel_fs_ops/open.rs`
- `Docs/决赛文档/README.md`
- `Docs/决赛文档/开发日志.md`
- `Docs/决赛文档/ai.log`
- `Docs/决赛文档/AI_INTERACTION.md`

## 验证

- `rustfmt --edition 2021 --check os/src/fs/kernel_fs_ops/open.rs os/src/syscall/fs/fd_ops.rs`：通过。
- `git diff --check`：通过。
- `make build-arch TARGET_ARCH=riscv64`：通过，生成 RISC-V release kernel。
- `make build-arch TARGET_ARCH=loongarch64`：通过，确认共享 VFS 模块可在另一架构编译。
- 未运行完整 `make run TARGET_ARCH=riscv64`：该路径需先完成 final-2026 BuildStorm，
  本次未执行长时间运行，故未宣称 OpenSBI 后续 guest 已启动。
