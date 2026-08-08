# BuildStorm 跨进程 unlink 后 fstat 返回 ENOENT

## 背景

BuildStorm 会并行启动 Cargo/rustc 子进程，共享 `target/debug/incremental` 下的增量缓存。
这些进程可能在一个进程仍持有文件描述符时，由另一个进程清理同一路径。

## 现象

`server.ans` 在编译 `axbuild` 时报告：

```text
[ERROR] [HART0] [PID 1112] [TID 1114] ext4_stat_get: rc = 2
[WARN] [HART0] [PID 1112] [TID 1114] Ext4Inode::fstat: ext4_stat_get failed rc=2, path="/work/tgoskits/target/debug/incremental/axbuild-3n5p0sz3bjvhw/s-hi3rlvmgwn-0cxktjd-working/query-cache.bin"
```

`rc = 2` 是 `ENOENT`。错误集中在 `query-cache.bin` 被并发清理后，仍持有该文件的
打开描述执行 `fstat(2)` 的路径。Linux 语义要求 unlink 只移除目录项，已有 fd 仍可
继续 `fstat` 和访问文件；因此这不是普通的“文件不存在”返回。

## 分析

原 `sys_unlinkat()` 先通过 `open()` 建立临时 `OSFile`，再调用当前进程的
`FSInfo::has_fd(&abs_path)` 判断是否存在打开 fd。`FSInfo` 是进程本地状态：当真正的
打开者是 Cargo/rustc 的兄弟进程时，该判断为假。unlink 随后立即删除目录项并失效
路径和 inode 缓存，兄弟进程持有的 `Ext4Inode` 仍带有原路径；它之后执行 `fstat()`
时，lwext4 的路径式 `ext4_stat_get()` 已无法找到目录项，于是返回 `ENOENT`。

该时序由新增的最小回归复现：子进程打开文件并等待，父进程 unlink，子进程再对仍持有
的 fd 调用 `fstat`。对最近的 MM/页表改动进行对照后，错误只在跨进程 unlink/fstat
时序出现，未发现页表或缺页路径破坏 EXT4 元数据；BuildStorm 的并发清理只是让原有
的进程本地 fd 判断缺陷稳定暴露。

## 根因

unlink 延迟删除的判断范围错误：它只查询当前进程的路径 fd，而没有统计所有仍引用同一
VFS inode 对象的打开文件描述。于是跨进程打开引用被误判为不存在，底层 inode 在仍被
使用时失去可解析路径。

## 修复

- 在 `os/src/fs/files/os_file.rs` 增加全局 `OPEN_FILE_COUNTS`，以共享 VFS inode
  `Arc` 的对象地址为 key，分别在 `OSFile` 构造和 `Drop` 时登记/释放引用。
- 提供 `OSFile::has_other_open_reference()`，让 unlink 判断排除自身的临时查找引用，
  只在 link count 为 1 且存在其他打开引用时调用 `inode.delay()`。
- 在 `os/src/syscall/fs/ctl/unlink.rs` 移除进程本地 `FSInfo::has_fd` 作为延迟删除依据，
  保留目录检查、dentry/inode cache 失效和普通 unlink 语义。
- 新增 `user/src/bin/initproc/fstat_unlink_regression.rs`，并在 final 测试入口加入
  跨进程 unlink/fstat 回归。未修改 ext4/lwext4 核心实现。

使用 inode `Arc` 对象身份而不是 inode number，避免延迟删除对象释放后 inode number
复用造成计数串扰。

## 涉及文件

- `os/src/fs/files/os_file.rs`
- `os/src/syscall/fs/ctl/unlink.rs`
- `user/src/bin/initproc.rs`
- `user/src/bin/initproc/fstat_unlink_regression.rs`

## 验证

此前短时 RISC-V QEMU 回归中，新增项输出 `fstat unlink regression: PASS`，随后
`sigaltstack`、`rseq` 和全部 CAgent 项也通过；原 `ext4_stat_get: rc = 2` 与对应
`Ext4Inode::fstat` warning 未再次出现。

完整 RISC-V BuildStorm 尚未在本轮确认，留待维护者后续运行；本轮只补充文档，没有
重新启动验证命令。
