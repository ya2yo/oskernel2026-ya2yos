# BuildStorm MemorySet 与 EXT4 可睡眠锁死锁

## 背景

BuildStorm 并发编译期间，lwext4 全局锁 `Ext4OpLock` 已改为竞争时通过
`PollSet + block_on()` 阻塞等待，避免其他 hart 长时间自旋。该锁因此可以在获取失败时
调度当前任务，调用方不能在持有其他资源内部锁时进入文件系统。

`os/src/task/mod.rs` 规定的任务侧顺序仍为
`TaskControlBlockInner -> ResourceSlot(get/replace) -> MemorySet`；`ResourceSlot` 仅用于
瞬时复制资源 `Arc`，不得跨用户内存访问或资源内部锁保留。

## 现象

`log.ans` 停在 BuildStorm 编译阶段。`gdbclient.ans` 的 4 至 8 号 hart 分别停在不同的
`MemorySet::get_ref()`：

```text
spin::rwlock::RwLock<MemorySetInner>::read
MemorySet::get_ref
rseq_prepare_user_return -> copy_to_user

MemorySet::get_ref
MemorySet::activate
trap_return
```

这些读锁等待的地址互不相同，表明并非单个全局 `MemorySet` 的普通竞争，而是多个任务各自
地址空间的写锁都没有释放；其余 hart 已空闲，不能再调度持锁任务推进。

## 分析

原有路径中，`MemorySet` 写锁会直接进入文件系统：

- `MmapFile::new()` 在 `mmap`/`mprotect` 的写锁范围内调用 `inode.size()`；
- 文件 mmap 缺页、EOF 判断和 fork 的 `MAP_SHARED` 预填充在写锁内调用
  `inode.size()`、`read_at()` 或 `FILE_PAGE_CACHE.get_or_load()`；
- `munmap()` 与 `recycle_data_pages()` 在写锁内执行 `link_cnt()`、`lseek()` 和 `write()`。

这些调用竞争 `Ext4OpLock` 时会进入 `block_on()`。于是任务被阻塞时仍持有自己的
`MemorySet` 写锁；该任务或同进程线程在调度返回、rseq 状态回写、页表激活时又需要读同一
`MemorySet`，形成不可恢复的自等待。其他 hart 看到的正是这些读锁自旋栈。

此前 `clone_process()` 还曾在持有 `TaskControlBlockInner` 时复制地址空间和访问用户内存。
该问题独立于本次 EXT4 死锁：修复后仍在持有 PCB 锁时按既定顺序瞬时读取
`memory_set_arc()`，随后释放 PCB 锁才进入 `MemorySet`；变量 tuple 声明保持原样。

## 修复

将可能进入 EXT4 的操作改为锁外阶段，将页表和 VMA 修改保留在短暂的 `MemorySet` 锁内阶段：

- 删除 `MmapFile::new()` 中无必要的 `inode.size()`；
- `MemorySet::handle_page_fault()` 先在读锁中复制 inode/page-index，再释放锁并加载
  `FILE_PAGE_CACHE`，最后以写锁只安装已缓存页；EOF 由缓存页的有效长度判断；
- `from_existed_user()` 预先加载 `MAP_SHARED` 的文件页，之后才取得父地址空间写锁完成
  共享页映射和 COW 建立；
- `munmap()` 和进程退出回收先在读锁中快照 `OSFile`、文件偏移和 `Arc<FrameTracker>`，
  释放锁后写回；回收阶段才重新取得写锁删除 VMA/页表；
- `clone_process()` 仅在 `TaskControlBlockInner` 锁内获取资源槽中的 `MemorySet` Arc，地址
  空间复制、fd/fs/signal 复制和 `CLONE_PARENT_SETTID` 用户内存写入均在 PCB 锁外执行。

这样 `Ext4OpLock` 可以正常阻塞和唤醒，而不会遗留 `MemorySet` 写锁。

## 涉及文件

- `os/src/task/task/task.rs`
- `os/src/mm/map_area.rs`
- `os/src/mm/page_fault_handler.rs`
- `os/src/mm/memory_set/fork_clone.rs`
- `os/src/mm/memory_set/handle.rs`
- `os/src/mm/memory_set/mmap_ops.rs`
- `os/src/mm/memory_set/accessors.rs`

## 验证

- `git diff --check` 通过；
- `cargo fmt --manifest-path os/Cargo.toml -- --check` 通过；
- `make TARGET_ARCH=riscv64` 通过，包含 RISC-V 与 LoongArch64 release 构建；仅有既有
  `smoltcp` 未使用项 warning；
- 沙箱内 `make run` 不能创建 QEMU 所需 `/var/tmp/vl.*` 临时文件；宿主 QEMU 回归在本轮未
  由 AI 完成。维护者随后确认死锁问题已修复。
