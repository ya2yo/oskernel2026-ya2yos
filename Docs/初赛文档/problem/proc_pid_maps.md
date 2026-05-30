# /proc/{pid}/maps 实现

## 概述

`/proc/{pid}/maps` 是 Linux procfs 中用于展示进程虚拟内存映射的文件。LTP `accept03` 测试会尝试 `open("/proc/self/maps")` 检测进程内存信息，内核需要在进程创建时生成该文件。

本文档介绍 Ya2yOS 内核中 `/proc/{pid}/maps` 的完整实现链路。

## 调用链路

```rust
用户态: open("/proc/self/maps")
  → sys_openat()                          [fd_ops.rs]
    → 路径翻译: /proc/self/maps → /proc/{pid}/maps
    → open()                              [kernel_fs_ops/open.rs]
      → root_inode().find()               [ext4 文件系统查找]
        → 找到 /proc/{pid}/maps inode ✓

进程创建时（fork/clone）:
  → clone_process()                       [task.rs]
    → create_proc_dir_and_file(pid, ppid, &memory_set)  [proc_file.rs]
      → 创建 /proc/{pid} 目录
      → 创建 /proc/{pid}/stat
      → 创建 /proc/{pid}/maps  ← 本次新增
```

## 实现要点

### 1. 路径翻译 (`fd_ops.rs`)

`/proc/self` 是进程自身的符号链接，内核需要将其替换为实际 PID：

```rust
// sys_openat 中
if abs_path == "/proc/self/stat" {
    abs_path = format!("/proc/{}/stat", task.pid());
}
if abs_path == "/proc/self/maps" {          // 新增
    abs_path = format!("/proc/{}/maps", task.pid());
}
```

> **注意**：此处使用 `if` 而非 `else if`，因为两个条件互斥，独立判断更清晰。

### 2. maps 文件内容格式 (`proc_file.rs`)

遵循 Linux `/proc/pid/maps` 标准格式：

```c
地址范围           权限  偏移量   设备号  inode   路径
<start>-<end>      rwxp  <offset> 00:00  0       <pathname>
```

#### 权限字符转换

从 `MapPermission` 位标志转换为 Linux 风格的 `rwxp` 字符串：

| MapPermission 位 | 含义 | 字符 |
| :-: | :-: | :-: |
| R (1<<1) | 可读 | `r` 或 `-` |
| W (1<<2) | 可写 | `w` 或 `-` |
| X (1<<3) | 可执行 | `x` 或 `-` |
| - | 私有映射 | 固定 `p` |

```rust
fn format_map_perm(perm: MapPermission) -> String {
    let mut s = String::with_capacity(4);
    s.push(if perm.contains(MapPermission::R) { 'r' } else { '-' });
    s.push(if perm.contains(MapPermission::W) { 'w' } else { '-' });
    s.push(if perm.contains(MapPermission::X) { 'x' } else { '-' });
    s.push('p');  // 当前所有映射均为私有
    s
}
```

#### 地址计算

`VirtPageNum` 为页号，需乘以 `PAGE_SIZE`(4096) 转换为字节地址：

```rust
let start = area.vpn_range.start().0 * PAGE_SIZE;
let end = area.vpn_range.end().0 * PAGE_SIZE;
```

#### 完整生成逻辑

```rust
let mut mapsinfo = String::new();
for area in &memory_set.get_ref().areas {
    let start = area.vpn_range.start().0 * PAGE_SIZE;
    let end = area.vpn_range.end().0 * PAGE_SIZE;
    let perm = format_map_perm(area.map_perm);
    let offset = area.mmap_file.offset;
    let pathname = if let Some(ref file) = area.mmap_file.file {
        file.inode.path()
    } else {
        String::new()
    };
    mapsinfo.push_str(&format!(
        "{:016x}-{:016x} {} {:08x} 00:00 0 {}\n",
        start, end, perm, offset, pathname
    ));
}
```

### 3. 文件写入方式 (`proc_file.rs`)

由于内核态写入文件使用 `UserBuffer`，需要将 `String` 转换为裸指针切片：

```rust
let mut mapsvec = Vec::new();
unsafe {
    let maps = mapsinfo.as_bytes_mut();
    mapsvec.push(core::slice::from_raw_parts_mut(
        maps.as_mut_ptr(),
        maps.len(),
    ));
}
let mapsbuf = UserBuffer::new(mapsvec);
mapsfile.write(mapsbuf)?;
```

> 此模式与已有的 `/proc/{pid}/stat` 写入方式完全一致。

### 4. 函数签名变更

`create_proc_dir_and_file` 需要获取进程的内存布局信息来生成 maps 内容：

```rust
// 之前
pub fn create_proc_dir_and_file(pid: usize, ppid: usize) -> Result<(), SysErrNo>;

// 之后
pub fn create_proc_dir_and_file(
    pid: usize,
    ppid: usize,
    memory_set: &MemorySet,
) -> Result<(), SysErrNo>;
```

调用方在 fork 流程中传入子进程的 `MemorySet`：

```rust
// task.rs - clone_process() 中
if flags.contains(CloneFlags::SIGCHLD) {
    let child_proc = child.process.inner_lock();
    let child_mm = child_proc.get_locked_memory_set_read();
    create_proc_dir_and_file(pid, ppid, &child_mm);
}
```

> **锁顺序说明**：此处需要同时持有 `process.inner_lock()`（MutexGuard）和 `memory_set`（RwLockReadGuard），前者保证 ProcessInner 数据一致性，后者保证内存布局在读取期间不被修改。

### 5. 清理逻辑

进程退出时，`remove_proc_dir_and_file` 需同步清理 maps 文件：

```rust
pub fn remove_proc_dir_and_file(pid: usize) {
    // 新增：清理 maps
    superblock_root_inode().unlink(format!("/proc/{}/maps", pid).as_str());
    FsIndex::remove_inode_idx(format!("/proc/{}/maps", pid).as_str());
    // 原有：清理 stat 和目录
    superblock_root_inode().unlink(format!("/proc/{}/stat", pid).as_str());
    FsIndex::remove_inode_idx(format!("/proc/{}/stat", pid).as_str());
    superblock_root_inode().unlink(format!("/proc/{}", pid).as_str());
    FsIndex::remove_inode_idx(format!("/proc/{}", pid).as_str());
}
```

> `FsIndex::remove_inode_idx` 用于淘汰全局 inode 缓存，防止已删除文件占用内存。

## 涉及的数据结构

| 结构 | 字段 | 说明 |
| :-- | :-- | :-- |
| `MemorySet` | `inner: SyncUnsafeCell<MemorySetInner>` | 进程地址空间 |
| `MemorySetInner` | `areas: Vec<MapArea>` | 所有内存区域列表 |
| `MapArea` | `vpn_range: VPNRange` | 虚拟页号范围 |
| `MapArea` | `map_perm: MapPermission` | 读写执行权限 |
| `MapArea` | `mmap_file: MmapFile` | mmap 文件信息（路径+偏移） |
| `MapArea` | `area_type: MapAreaType` | 区域类型（Elf/Stack/Brk/Mmap/Shm 等） |
| `VPNRange` | `start()/end() → VirtPageNum` | 起始/结束虚拟页号 |
| `VirtPageNum` | `.0: usize` | 页号（× PAGE_SIZE = 字节地址） |

## 示例输出

一个典型进程的 `/proc/{pid}/maps` 内容：

```c
0000015000000000-0000015000001000 r-xp 00000000 00:00 0 /initproc
0000015000001000-0000015000002000 rw-p 00000000 00:00 0
000002fff4476000-000002fff4497000 rw-p 00000000 00:00 0
ffffffffff610000-ffffffffff611000 r-xp 00000000 00:00 0
```

- 第一行：ELF 代码段（`r-xp`），来自 `/initproc`
- 第二行：ELF 数据段（`rw-p`），匿名映射
- 第三行：用户栈（`rw-p`）
- 第四行：vdso / 跳板页（`r-xp`）

## 未来改进方向

1. **动态更新**：当前 maps 在进程创建时生成并写入磁盘文件，进程后续 `mmap`/`munmap`/`brk` 等操作不会更新该文件。理想方案是实现 procfs 的按需读取（read 时动态生成内容），而非静态写入。
2. **完整权限**：当前固定使用 `p`(private)，实际应考虑 `MAP_SHARED` 标志输出 `s`(shared)。
3. **设备号和 inode**：当前固定 `00:00 0`，应结合实际块设备信息填充。
4. **更多区域类型标注**：如 `[stack]`、`[heap]`、`[vdso]` 等 Linux 风格的区域名称标注。
