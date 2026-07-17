# LTP fs_bind rbind 挂载传播与 BusyBox applet 缺失修复

## 背景

LTP `fs_bind` 系列通过 `mount --bind`、`mount --rbind` 和 `--make-rshared` 等操作检查 mount propagation。维护者在解除相关 blacklist 并单跑 `fs_bind_rbind01.sh` 后发现，测例先因缺少 `seq` 只能执行前两项；补齐后，新的 `log.ans` 显示 shared peer 间的挂载传播均失败。

## 现象

`fs_bind_rbind01` 的 `mount` 命令曾全部返回成功，但以下断言失败：

- `parent2` 与其 rbind 副本 `share2` 内容不同；
- `parent1/child1` 的挂载没有传播到 `parent2/child2`、`share2/child2`；
- 后续挂载 `disk2`、`disk3` 没有出现在 shared peer 的相对路径；
- 原日志中 `TFAIL: "..." differ:` 后没有 diff 内容。

## 分析

竞赛镜像的 BusyBox 已以 `CONFIG_SEQ=y` 构建，但启动期 `BUSYBOX_APPLETS` 未创建 `/bin/seq -> /musl/busybox`。`fs_bind` 的 shell 依赖 `seq`，因此会在循环位置提前停止。

同一清单还遗漏 `/bin/diff`。`fs_bind_check()` 执行 `diff -r` 时把 stderr 重定向到 `/dev/null`；命令不存在时返回 127，于是 LTP 只记录空的 `differ:` 文本，掩盖了实际缺失 applet。

更深层的传播失败来自旧 `MountTable`：它仅用四元组保存单个挂载元数据，目标路径已有记录时直接成功返回；既不保留 mount stack，也不记录 `MS_SHARED` peer group，更不会为 `MS_BIND | MS_REC` 建立路径视图或向 peer 复制新挂载事件。由于当前 VFS 仍是路径化查找，单纯记录元数据无法使 `diff -r` 观察到 bind tree。

## 根因

启动期命令链接不完整与挂载表语义缺失叠加：前者阻止测例完整执行，后者让 `--make-rshared` 和 `--rbind` 成为无可观察效果的成功操作。

## 修复

- 在 `BUSYBOX_APPLETS` 中加入 `/bin/seq` 和 `/bin/diff`。
- 将 `MountTable` 改为分层 `MountEntry`，保存源、目标、flags、shared group 和 mount event group，并将容量从 16 提升至 256。
- `--make-{shared,private,slave,unbindable}` 按 `MS_REC` 更新相应挂载范围；新 bind 事件根据父挂载 shared group 在 peer 的同一相对路径创建副本。
- `umount` 只回收本次事件产生的 peer 副本，保留同一路径的更早挂载层，匹配 rbind01 的多次卸载顺序。
- `sys_mount()` 对 bind source 使用当前 cwd 归一化为绝对路径；在不具备真实 mount-root dentry 的现有 VFS 中，将 source tree 镜像到每个目标路径。目录递归创建，普通文件使用 hard link 保持共享 inode 语义。

这是一层针对当前路径化 VFS 的兼容实现，不改变非 bind 文件系统挂载的 source 解释方式；完整 mount namespace/VFS mount-root 仍是后续架构工作。

## 涉及文件

- `os/src/fs/kernel_fs_ops/initfiles.rs`
- `os/src/fs/mount.rs`
- `os/src/syscall/fs/mount.rs`

## 验证

- `rustfmt --edition 2018 --check os/src/fs/mount.rs os/src/syscall/fs/mount.rs` 通过。
- `make TARGET_ARCH=riscv64` 完成 RISC-V 和 LoongArch64 release 构建；仅有既有 vendored `smoltcp` warning。
- 在允许 QEMU 使用 `/var/tmp` 的环境中执行 `timeout 180s make run TARGET_ARCH=riscv64`。RISC-V `fs_bind_rbind01` 的 musl、glibc 均为 `passed 28 failed 0 broken 0 skipped 0 warnings 0`，所有 propagation comparison 与卸载步骤 `TPASS`，并正常 `shutdown!`。
- 维护者当前 `initproc.rs` 仅配置 `fs_bind_rbind01`；其余 `fs_bind`/`rbind` 组合尚未在本轮运行。
