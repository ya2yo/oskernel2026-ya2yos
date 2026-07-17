# LTP fs_bind23 MS_MOVE 子树重定位与 shared peer 传播

## 背景

LTP `fs_bind23.sh` 验证已包含 shared child 的挂载树被 `mount --move` 移入 shared 父挂载时，移动后的树以及目标父挂载的 peer 均能观察到全部子挂载。当前 Ya2yOS 以 `MountTable` 保存挂载元数据，并在 syscall 层镜像目录树，使路径化 VFS 能观察 bind/传播结果。

## 现象

原始 `log.ans` 中，前半段 `mnt/1 -> mnt/2` 的 shared child 传播全部通过，但执行：

```sh
mount --move mnt tmp1/3
```

后，`tmp1/3/1/abc` 不存在，`tmp2/3/1/abc` 与 `tmp2/3/2/abc` 也无法检查。后续对这些路径的 `umount` 返回 `EINVAL`，`/proc/mounts` 最终残留旧的 `mnt`、`mnt/1`、`mnt/2` 及其子挂载。

## 分析

`MountTable::mount()` 只区分 propagation、remount 和 bind。带 `MS_MOVE` 的 legacy `mount(2)` 调用误落入普通挂载分支：它新建了一条目标记录，却没有移除或重定位 source subtree 的既有条目。与此同时，syscall 层仅根据返回的 bind copy 做 `mirror_bind_tree()`；普通分支没有返回任何路径对，因此路径化 VFS 下 `tmp1/3` 也没有得到源目录视图。

在本用例中，目标 `tmp1/3` 的父挂载 `tmp1` 是 shared，`tmp2` 是其 peer。移动根以及 `mnt/1`、`mnt/2` 和 `abc` 子挂载都必须重定位到 `tmp1/3`，并在 `tmp2/3` 建立同 event group 的副本，才能保持后续逐层卸载的现有模型。

## 根因

路径化挂载实现缺少 `MS_MOVE` 的专用语义：没有重写已存在 mount subtree 的路径，也没有将 move event 展开到目标父挂载的 shared peer/slave 接收者，更没有同步目录视图。`MS_MOVE` 被错误建模为一个无关联的普通挂载。

## 修复

- 新增 `MountTable::move_mount()`，要求 source 是精确的可见 mountpoint，并拒绝将挂载移动到自身子树。
- 将 source subtree 原地重定位到目标路径；目标父挂载存在 shared peer/slave 时，为每个接收目标复制整个 subtree。
- 副本保留原有 `event_group`、shared/slave 状态，因此从任一副本执行 `umount` 都能回收同一原始 mount event 的所有副本。
- `MS_MOVE` 的 source pathname 与 bind 一样经 `get_abs_path()` 归一化；挂载表向 syscall 层返回每个 `(source, target)`，复用既有 `mirror_bind_tree()` 让目标目录树对路径查找可见。

该修复仍遵循当前路径化 VFS 的兼容模型，并未实现完整 Linux VFS 的独立 mount-root dentry 或 mount namespace。

## 涉及文件

- `os/src/fs/mount.rs`
- `os/src/syscall/fs/mount.rs`

## 验证

- `rustfmt --edition 2018 --check os/src/fs/mount.rs os/src/syscall/fs/mount.rs` 与 `git diff --check` 通过。
- 根目录 `make` 完成 RISC-V 和 LoongArch64 release 构建；仅有既有 vendored `smoltcp` warnings。
- 在允许 QEMU 写入 `/var/tmp` 的环境中执行 `timeout 180s make run TARGET_ARCH=riscv64`。维护者的单测入口分别运行 musl/glibc `fs_bind23.sh`，两者均为 `passed 20 failed 0 broken 0 skipped 0 warnings 0`；move 后三路径 propagation 和全部六次 `umount` 均为 `TPASS`，最终正常 `shutdown!`。
- LoongArch64 未单独运行该 LTP 单测。
