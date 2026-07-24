# LTP mmap16 ext4 loop 容量与 mmap 写回修复

## 背景

`mmap16` 回归测试验证 ext4 在块设备空间耗尽时，`MAP_SHARED` 文件映射不会静默丢失数据。测试使用 1 KiB block、约 10 MiB 的 `/dev/loop0` 文件系统：父进程持续写入直到 `ENOSPC`，子进程扩展并写入共享映射，预期子进程因写回失败收到 `SIGBUS`。

## 现象

初始日志中 `mremap(addr, old, new, 0)` 返回 `ENOSYS`。补齐基础语义后，musl/glibc 均可得到 `TPASS`，但每个 30 秒 LTP 运行窗口只完成约 5 个轮次，随后输出 `TBROK: Test killed! (timeout?)`；没有 `TFAIL`。

## 分析

1. `sys_mremap()` 将所有未设置 `MREMAP_MAYMOVE` 的请求直接返回 `ENOSYS`，缺少 Linux 的原址扩展和收缩语义。
2. 共享映射在解除映射时写回文件页；底层空间不足应转换为进程收到的 `SIGBUS`，不能把 `munmap()` 当作普通错误返回。
3. 简化 loop 设备丢弃格式化写入内容，ext4 挂载仍共用根 superblock，父进程无法观察到独立 loop 文件系统的容量边界，导致测试无法稳定触发目标 `ENOSPC`。
4. whole-file write-back cache 在父进程的 1 KiB 连续写入中重复执行路径查找、seek 和缓存表查询；达到容量后关闭 inode 还会同步把整份脏缓存写入简化的真实 ext4，浪费 LTP checkpoint 的时间窗口。

## 根因

缺少的是多个简化层之间的契约，而不是 mmap 单一页表错误：VMA 调整、共享映射写回错误、loop 格式化容量和路径化 ext4 的空间账本没有连起来；同时写入热路径和关闭路径仍按真实磁盘持久化处理，放大了测试负载。

## 修复

- `MemorySetInner::mremap_in_place()` 实现完整 VMA 的原址扩展、收缩、相邻区域冲突和 `MAX_MMAP_SIZE` 检查；`sys_mremap()` 在未设置 `MAYMOVE` 时调用该路径。
- `sys_munmap()` 将共享文件写回的 `ENOSPC` 转为当前线程的 `SIGBUS` 并完成解除映射。
- loop 设备记录格式化阶段写入的最大 offset；ext4 挂载把该容量接入共享的 `MountUsage` 账本。由于当前 VFS 没有独立块组分配器，账本按格式化容量约三分之一作为可用普通文件数据空间，并保留至少 1 MiB；文件增长按 64 KiB 预留、失败回滚，unlink/延迟 unlink 释放账本。
- whole-file cache 上限提高到 16 MiB；新增按显式 offset 写入的快路径，合并 cache lookup、offset 更新和写入，且复用已打开的 ext4 descriptor。
- 逻辑配额耗尽后保留脏缓存供内核内读取，关闭时跳过无法落盘的整文件写回；显式同步仍走正常写回，最后一个目录项删除时丢弃缓存。
- `mkfs.ext4` 启动文件包装为 `mke2fs`，使 LTP 的格式化阶段在当前用户镜像中可执行。

## 涉及文件

- `os/src/syscall/mm/mmap.rs`
- `os/src/mm/memory_set/mmap_ops.rs`
- `crates/lwext4_rust/src/file.rs`
- `os/src/fs/ext4_lw/inode.rs`
- `os/src/fs/files/loopdev.rs`
- `os/src/fs/mount.rs`
- `os/src/syscall/fs/mount.rs`
- `os/src/syscall/fs/ctl/unlink.rs`
- `os/src/fs/kernel_fs_ops/initfiles.rs`

## 验证

- `make riscv64-build`：通过。
- `timeout 180s make run TARGET_ARCH=riscv64 > /tmp/mmap16-third-capacity.log 2>&1`：musl/glibc 均 `passed 10 failed 0 broken 0 skipped 0 warnings 0`，均输出 `RESULT ... : 0`，最终 `shutdown!`；无 `TFAIL`、`TBROK`、panic 或错误日志。
- `make loongarch64-build`：通过；未运行 LoongArch64 的 mmap16 QEMU 单项。
