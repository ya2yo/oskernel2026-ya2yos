# clone03 fork 后 MAP_SHARED 物理帧未共享

### 背景

LTP `clone03` 测试验证 fork 后父子进程通过 `MAP_SHARED | MAP_ANONYMOUS` 区域通信：子进程写入 `getpid()`，父进程校验 `clone()` 返回值与子进程写入的 PID 一致。同时内核在 `recycle_data_pages` 中对 MAP_ANONYMOUS 区域调用 `unwrap()` 触发 panic。

### 现象

**clone03 retval=4 失败：**

```
clone03.c:38: TFAIL: pid(0) retval 4 != 0: SUCCESS (0)
```

clone() 返回 4（子进程 PID=4 正确），但 `*child_pid` 读到 0——子进程写入的值父进程不可见。

**recycle_data_pages panic：**

当进程退出时，对 MAP_SHARED | MAP_ANONYMOUS 区域的回写逻辑触发：

```
[kernel] Panicked at src/mm/memory_set/mod.rs:481
called `Option::unwrap()` on a `None` value
```

### 分析

**retval=4 的根因在 `from_existed_user`（fork_clone.rs）：**

```rust
// fork 时 MAP_SHARED 区域的原有处理
if area.mmap_flags.contains(MmapFlags::MAP_SHARED) {
    let frames = area.data_frames.values().cloned().collect();
    memory_set.push_with_given_frames(new_area, frames);
    continue;
}
```

mmap 采用懒分配：父进程 fork 前未访问过 MAP_SHARED 页面时，`data_frames` 为空。`frames` 是空 Vec，子进程得到的区域没有物理帧。

随后：
1. 子进程 `*child_pid = getpid()` → StorePageFault → 分配**独立的**新帧，写入 PID=4
2. 父进程读 `*child_pid` → LoadPageFault → 分配**另一个**新帧（全 0），读到 0

MAP_SHARED 语义被破坏：父子各自持有独立物理帧，写入不互通。

**panic 的根因在 `recycle_data_pages`（mod.rs:467-496）：**

```rust
for area in self.areas.iter_mut() {
    if area.area_type == MapAreaType::Mmap {
        if area.mmap_flags.contains(MmapFlags::MAP_SHARED)
            && area.map_perm.contains(MapPermission::W)
        {
            let file = area.mmap_file.file.clone().unwrap(); // PANIC: file 是 None
```

仅检查了 `MAP_SHARED` 和 `W` 权限，但未检查 `MAP_ANONYMOUS`。对于 `MAP_SHARED | MAP_ANONYMOUS` 区域，`mmap_file.file` 为 `None`，`unwrap()` 直接 panic。

### 修复

**fork_clone.rs — 预 fault pass：**

在 `from_existed_user` 主循环之前，新增一段遍历，将所有 MAP_SHARED 区域中尚未分配物理帧的页面提前 fault in：

- **MAP_ANONYMOUS**：调用 `map_one()` 分配零页
- **文件支撑**：调用 `mmap_write_page_fault()` 从文件读取数据到帧

这样 `data_frames` 在 fork 时必定非空，后续帧共享才生效。

**mod.rs — 增加 MAP_ANONYMOUS 检查：**

```rust
if area.mmap_flags.contains(MmapFlags::MAP_SHARED)
    && area.map_perm.contains(MapPermission::W)
    && area.mmap_file.file.is_some()  // 跳过 MAP_ANONYMOUS
```

### 涉及文件

- `os/src/mm/memory_set/fork_clone.rs` — 新增预 fault pass
- `os/src/mm/memory_set/mod.rs` — `recycle_data_pages` 加 `is_some()` 检查

### 验证

RISC-V 单跑 `clone03`：TPASS。
