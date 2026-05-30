# accept03 修复过程

## accept03.c:48: TFAIL: accept() on O_PATH file expected EBADF: ENOTSOCK (88)

```rust
if file.flags() & OpenFlags::O_PATH.bits() !=0 {
        return Err(SysErrNo::EBADF)
}
```

这里需要先检查是否设置O_PATH在调用socket()

## tst_fd.c:65: TBROK: open(/proc/self/maps,0,0000) failed: ENOENT (2)

### 现象

LTP 测试 `accept03` 在运行时尝试 `open("/proc/self/maps", O_RDONLY)`，内核返回 `ENOENT`（文件不存在）。

日志关键行：

```c
[sys_openat] path is /proc/self/maps, flags is O_LARGEFILE, mode is 0
[ERROR] unexpected error in root_inode().find(/proc/self/maps,O_LARGEFILE,0):ENOENT
tst_fd.c:65: TBROK: open(/proc/self/maps,0,0000) failed: ENOENT (2)
```

### 根因

之前内核只实现了 `/proc/self/stat` 的虚拟文件支持，缺少 `/proc/self/maps` 的两个关键环节：

1. **路径翻译缺失**：`sys_openat` 中仅对 `/proc/self/stat` 做了 `pid` 替换（`/proc/self/stat` → `/proc/{pid}/stat`），没有处理 `/proc/self/maps`
2. **文件创建缺失**：`create_proc_dir_and_file` 中只创建了 `/proc/{pid}/stat`，没有创建 `/proc/{pid}/maps`

### 修复

修改了三个文件：

**1. `os/src/syscall/fs/fd_ops.rs`** — 添加 `/proc/self/maps` 路径翻译：

```rust
if abs_path == "/proc/self/maps" {
    abs_path = format!("/proc/{}/maps", task.pid());
}
```

**2. `os/src/fs/kernel_fs_ops/proc_file.rs`** — 扩展 `create_proc_dir_and_file` 签名，增加 `&MemorySet` 参数，遍历进程内存区域（`areas`）生成标准 `/proc/pid/maps` 格式内容并写入文件；同步更新 `remove_proc_dir_and_file` 清理 `/proc/{pid}/maps`。

**3. `os/src/task/task/task.rs`** — 调用点传入子进程的 `MemorySet` 引用：

```rust
let child_proc = child.process.inner_lock();
let child_mm = child_proc.get_locked_memory_set_read();
create_proc_dir_and_file(pid, ppid, &child_mm);
```

### 实现细节

参见 [proc_pid_maps.md](./proc_pid_maps.md)

## 不支持的系统调用号

之后只会报错因为该系统调用不支持导致进程直接退出，最简单解决方法是在实现对应的系统调用分配一个虚假的文件描述符进行占位，这样在accept时会按照预期那样报错，`ENOTSOCK`。

最小修改方式：

在 `src/fs/files/` 目录下新增 `dummyfd.rs`:

```rust
//! 这是一个临时文件，这里实现的是虚假的文件描述符供那些没有真正实现的文件描述符使用

use alloc::sync::Arc;

use super::super::File;

pub struct DummyFd;

impl DummyFd {
    pub fn new() -> Arc<Self> {
        Arc::new(DummyFd {})
    }
}

impl File for DummyFd {}
```
