# LTP fs_bind13 unbindable bind source 语义修复

## 背景

LTP `fs_bind13.sh` 验证 shared parent 下的 unbindable child：被 `mount --make-runbindable` 标记的挂载不能作为后续 `mount --bind` 的 source。

## 现象

新的 `log.ans` 中，`mount --bind parent1/child1 parent2/child2` 被 LTP 标记为 `EXPECT_FAIL`，但内核返回成功。后续 cleanup 还发现 `parent2/child2`、`share2/child2` 残留在 `/proc/mounts`。

## 分析

此前为 fs_bind propagation 引入的 `MountEntry` 仅保存 `shared_group`。`set_propagation()` 将 `MS_UNBINDABLE`、`MS_PRIVATE` 和 `MS_SLAVE` 都表示为 `shared_group = None`，无法在下一次 bind 时区分 unbindable source。

Linux 在 legacy `mount(2)` 的 bind 路径调用 `do_loopback()`；若 source mount 为 unbindable，直接返回 `EINVAL`。检查必须发生在复制 mount tree、生成 shared peer 副本以及镜像路径树之前。

## 根因

挂载表丢失了 unbindable propagation type，导致 bind source 合法性未被检查，非法 bind 被登记并传播。

## 修复

- `MountEntry` 增加 `unbindable` 状态。
- 处理 `MS_UNBINDABLE` 时设置该状态；所有其他 propagation type 变更都会清除它。
- `MountTable::mount()` 在处理 `MS_BIND` 后、创建目标列表前检查 source 的顶层挂载。source 为 unbindable 时返回 `SysErrNo::EINVAL`。
- `MountTable::mount()` 的错误类型改为 `SysErrNo`，保留容量耗尽时的 `ENOSPC`，并让 syscall 层原样传播 `EINVAL`。

## 涉及文件

- `os/src/fs/mount.rs`
- `os/src/syscall/fs/mount.rs`

## 验证

- `rustfmt --edition 2018 --check os/src/fs/mount.rs os/src/syscall/fs/mount.rs` 通过。
- `make TARGET_ARCH=riscv64` 完成 RISC-V 和 LoongArch64 release 构建；仅有既有 vendored `smoltcp` warning。
- 在允许 QEMU 使用 `/var/tmp` 的环境中运行 `timeout 180s make run TARGET_ARCH=riscv64`。RISC-V `fs_bind13` 的 musl、glibc 均为 `passed 24 failed 0 broken 0 skipped 0 warnings 0`；预期失败的 bind 显示 `failed as expected`，无 cleanup 残留并正常 `shutdown!`。
- LoongArch64 未单独运行该 LTP 单测。
