# 内核堆碎片化：全量测例运行 OOM

### 现象

全部 11 组测例顺序运行，最后 `iozone-glibc` panic：

```c
[kernel] Panicked at src/mm/heap_allocator.rs:18 Heap allocation error,
         layout = Layout { size: 13770752, align: 1 (1 << 0) }
```

单独运行 `iozone-glibc` 正常通过（`no-log.ans`）。

### 根因

内核堆 48MB（buddy system allocator，2^n 分配）。iozone 申请 13.77MB → 向上取整为 16MB 块（2^24）。

单独运行时堆是干净的，16MB 块存在。全量运行 10 组测例后，**FsIndex 全局 inode 缓存** (`fs/kernel_fs_ops/fsidx.rs`) 持续增长：

- 每次 `open()` 将 inode 插入 `FSIDX`（`open.rs:12,48`）
- `sys_close` **不清理** FsIndex 条目
- 仅 `sys_unlinkat` 和 `remove_proc_dir_and_file` 会删除条目
- 10 组测例累积数百个 inode 条目（测试二进制、动态库、临时文件等），每条目的 `String` 路径 + `Arc<dyn Inode>` 占据堆空间

这些持久条目散布在堆中，阻止 buddy allocator 合并出 16MB 连续块。

### 修改点

**`os/src/syscall/fs/fd_ops.rs` — `sys_close` 增加 FsIndex 淘汰**

```rust
// 在关闭前获取 inode 路径，用于 FsIndex 缓存淘汰
let inode_path = fd_table
    .try_get(fd)
    .and_then(|desc| desc.file().ok())
    .map(|osfile| osfile.inode.path());

// ... fd_table.take(fd) ...

// 若该 inode 仅被全局缓存持有，则淘汰以释放堆内存
if let Some(path) = inode_path {
    if !path.is_empty() && !path.starts_with("/proc") {
        if let Some(inode) = FsIndex::find_inode_idx(&path) {
            // FsIndex + find_inode_idx 返回的 clone = 2 份引用
            if Arc::strong_count(&inode) <= 2 {
                FsIndex::remove_inode_idx(&path);
            }
        }
    }
}
```

逻辑：

1. 关闭 fd 前通过 `FileDescriptor::file()` 获取 `Arc<OSFile>`，从中取出 `inode.path()`
2. `fd_table.take()` 释放 OSFile → inode Arc 引用计数递减
3. 通过 path 在 FsIndex 中查找 inode，若 `Arc::strong_count <= 2`（仅 FsIndex + 我们的局部变量），说明没有其他进程持有该 inode，安全移除缓存条目

`/proc` 路径跳过，因其由 `remove_proc_dir_and_file` 在进程退出时专门清理。

---
