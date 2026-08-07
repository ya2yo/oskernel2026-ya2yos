# BuildStorm 零长度 EXT4 目录项死循环与损坏传播

## 背景

BuildStorm 在持久化 EXT4 镜像上会频繁创建和截断 `/proc/<pid>/maps` 等文件。lwext4 的目录插入路径需要遍历目标目录块，并使用每个目录项的 `rec_len` 推进游标；来自磁盘的长度字段不能未经校验地控制指针移动。

## 现象

最初的 `server.ans` 在输出 `BUILDSTORM_TOOLCHAIN ok` 后不再推进。GDB 反复命中：

```c
while (start < stop) {
	uint32_t inode = ext4_dir_en_get_inode(start);
	uint16_t rec_len = ext4_dir_en_get_entry_len(start);
	/* ... */
	start = (void *)((uint8_t *)start + rec_len);
}
```

加入目录项边界校验后，新的 `server.ans` 首先报告：

```text
ext4_fopen2_with_metadata: /proc/67/maps, rc = 5
```

调用路径为 `refresh_proc_maps -> open(O_CREATE|O_TRUNC|O_RDWR) -> create_with_metadata_impl -> ext4_fopen2_with_metadata -> ext4_dir_add_entry -> ext4_dir_try_insert_entry`。

## 断点与磁盘证据

在第一个 `EIO` 现场，目录插入参数为：

```text
parent inode = 22480
dst block = 490744
name = "maps"
block_size = 4096
inode = 0
rec_len = 0
existing_name_len = 0
remaining = 4096
```

该 4 KiB 目录块整块为零。`debugfs` 将 inode 22480 解析为 `/proc/67`，其大小为 8192 字节、extent 为 `(0-1):490744-490745`，两个物理块均为零。对原镜像执行只读 `e2fsck -fn` 也报告：

```text
Directory inode 22480, block #0, offset 0: directory corrupted
e2fsck: aborted
```

因此新的 `EIO(5)` 不是目录项校验误判，而是原始持久化镜像中的真实目录损坏。

## 根因

问题包含两个不同层面：

1. lwext4 的内核 bug：`ext4_dir_try_insert_entry()` 未验证 `rec_len`。当损坏目录项的 `rec_len == 0` 时，`start` 永远不变，内核在目录块遍历中永久循环。
2. 错误传播 bug：线性目录路径会在释放 block 后丢失插入错误，并继续扫描或追加目录块；HTree 路径会对非 `ENOSPC` 错误继续 split，且 cleanup 的成功返回可能覆盖真实插入失败。这些行为会隐藏介质或文件系统损坏。

原镜像还存在 journal 损坏。对单独复制到 `/tmp` 的镜像运行离线修复时，`e2fsck` 明确报告 `Journal transaction 260845 was corrupt, replay was aborted.`，并继续发现其他目录、HTree、extent、inode checksum 和块引用计数错误。这属于测试镜像状态，不应通过吞掉 `EIO` 或继续追加目录块来绕过。

## 修复

- `ext4_dir_try_insert_entry()` 在读取目录项后验证剩余空间、最小记录长度、4 字节对齐、块边界和 `name_len`；损坏记录返回 `EIO`，不再允许无效长度控制游标。
- `ext4_dir_add_entry()` 在释放 block 前保存插入结果；只有 `ENOSPC` 才继续扫描或分配新块，`EIO` 等错误立即传播。
- `ext4_dir_dx_add_entry()` 只有在插入返回 `ENOSPC` 时才分裂 HTree 数据块，并在 cleanup 后恢复原始插入结果。
- 增加 host 回归程序，覆盖 `rec_len` 为零、过短、未对齐、越过块边界以及 `name_len` 超过记录容量的情况。

## 验证

- host 回归 `lwext4-dir-entry-validation`：通过，输出 `lwext4-dir-entry-validation: PASS`。
- `make TARGET_ARCH=loongarch64`：仓库构建流程中的 RISC-V 与 LoongArch64 release 均通过，仅有既有 `smoltcp` unused warning。
- 原镜像的只读 `e2fsck -fn` 稳定报告 inode 22480 目录损坏；未对原镜像执行写入式修复。
- 仅在 `/tmp/buildstorm-dir-eio.img` 副本执行 `e2fsck -fy`。同一内核从该修复副本启动后输出 `BUILDSTORM_TOOLCHAIN ok`、`BUILDSTORM_MINIBUILD ok`，并在 120 秒窗口内推进到 Cargo `443/446`，没有再次出现 `/proc/67/maps, rc = 5` 或 `ext4_fremove ext4_dir_rm: rc = 5`。

修复副本的运行在 timeout 前没有完成 446 个 crate，因此不能据此宣称完整 BuildStorm 已通过。原始镜像仍需在备份后离线修复或替换；本次代码修复的保证是损坏目录项不会再卡死内核，并且真实文件系统错误不会被静默覆盖。
