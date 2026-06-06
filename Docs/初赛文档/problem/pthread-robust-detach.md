# pthread_robust_detach 测试修复：地址转换重构 + 退出/等待信号链修复

## 背景

`pthread_robust_detach` 是 LTP 中 pthread robust mutex 相关的测例。执行流程：

1. runtest.exe（PID 2）fork 出 entry-dynamic.exe（PID 3，带 `SIGCHLD`）
2. PID 3 内部创建线程（TID 4，`CLONE_THREAD`），线程持有 robust mutex
3. 线程/进程退出时触发 robust futex 链表清理（`handle_futex_when_exit`）
4. runtest.exe 通过 `sigtimedwait` + `waitpid` 回收子进程

本次修复涉及三个独立但相互关联的 bug，以及两项基础设施重构。

---

## Bug 1: 非法 VA 导致 `VirtAddr::from` panic

### 现象

```
[kernel] Panicked at src/mm/address.rs:103 invalid va: 0x2e417e0221140
```

### 分析

进程退出时 `handle_futex_when_exit` 遍历 robust futex 链表，链表被破坏（`next` 指针为垃圾值 `0x2e417e0221140` —— 非 SV39 规范地址）。`copy_from_user` 内部 `VirtAddr::from()` 遇到非法地址直接 `assert!` panic，而非返回错误。

日志证据：
```
handle_futex_when_exit: entry=0x1f180, futex_word=0x1f160, next=0x2e417e0221140
                                       ↑ 垃圾值，非规范 VA
```

### 修复

`os/src/mm/translate.rs` 中所有 `VirtAddr::from()` 替换为 `VirtAddr::try_from()`，非法 VA 返回 `EFAULT`：

| 函数 | 修改 |
|------|------|
| `checked_user_range` | 新增 VA 合法性预检 |
| `copy_from_user` 循环 | `VirtAddr::from` → `try_from().ok_or(EFAULT)?` |
| `copy_to_user` 循环 | 同上 |
| `read_user_cstr` | 同上，非法时先检查已读部分是否已有完整字符串 |
| `read_user_bytes_direct` / `write_user_bytes_direct` | 同上 |

修复后日志：
```
handle_futex_when_exit: invalid entry 0x2e417e0221140, stopping walk
handle_futex_when_exit: done
```
不再 panic，优雅退出。

---

## Bug 2: `sigtimedwait` 残留 `interrupted` 标志导致 `waitpid` 返回 `EINTR`

### 现象

```
SigTimedWait ret = Try again          ← EAGAIN（超时）
Wait4 ret = Interrupted system call   ← EINTR（异常）
src/common/runtest.c:91: waitpid failed: Interrupted system call
```

### 分析

`sigtimedwait` 超时时，`handle_sigtimedwait_timer` 调用了 `task.interrupt()` 设置 `interrupted = true`。但 `sigtimedwait` 返回 `EAGAIN` 后**没有清除**该标志。

随后 `waitpid` 使用 `block_on(interruptible(poll_fn(...)))`，`interruptible` 检查 `poll_interrupt` → 发现残留的 `interrupted` 标志 → 直接返回 `EINTR`，根本没机会检查子进程状态。

### 修复

`os/src/syscall/signal.rs` 的 `sys_rt_sigtimedwait` 在检测到超时后，调用 `task.clear_interrupt()` 清除残留标志：

```rust
if task_inner.sigtimedwait_timedout {
    task_inner.sigtimedwait_timedout = false;
    task.clear_interrupt();  // ← 新增
    drop(task_inner);
    return Poll::Ready(Err(SysErrNo::EAGAIN));
}
```

---

## Bug 3: `CLONE_THREAD` 覆盖 `exit_signal` 导致 `waitpid` 返回 `ECHILD`

### 现象

```
sys_waitpid: my children (pid, exit_sig, all_exited): [(3, -1, true)]
Wait4 ret = No child processes   ← ECHILD
src/common/runtest.c:91: waitpid failed: No child processes
```

PID 3 的 `exit_signal = -1`，而 `waitpid` 默认过滤要求 `exit_signal == SIGCHLD(17)`，匹配失败 → `ECHILD`。

### 分析

调用链：

```
PID 2 fork PID 3（SIGCHLD 标志）
  → clone_process 设置 PID 3 的 exit_signal = SIGCHLD(17)  ✓

PID 3 clone TID 4（CLONE_THREAD 标志）
  → clone_process 设置 exit_signal = -1（因为线程没有 SIGCHLD）
  → 但 CLONE_THREAD 时 child.process == self.process（同一进程）！
  → PID 3 的 exit_signal 从 17 被覆盖为 -1  ✗
```

根因：`clone_process` 的 exit_signal 设置代码没有区分 fork 和 thread。线程不应该覆盖进程级字段。

### 修复

`os/src/task/task/task.rs` 的 `clone_process`，exit_signal 设置加 `!CLONE_THREAD` 守卫：

```rust
// exit_signal: 仅 fork（非线程）才设置，线程共享进程不能覆盖已有值
if !flags.contains(CloneFlags::CLONE_THREAD) {
    let mut child_meta = child.process.meta_lock();
    child_meta.exit_signal = if flags.contains(CloneFlags::SIGCHLD) {
        SIGCHLD as i32
    } else {
        -1
    };
}
```

---

## 重构 1: mm/translate.rs 地址转换全面重构

### 背景

原有地址转换 API 存在两类问题：

1. **直接暴露 `&'static mut` 引用**：`translated_byte_buffer` / `safe_translated_byte_buffer` 返回 `Vec<&'static mut [u8]>`，将用户物理页的可变引用以 `'static` 生命周期暴露给内核任意代码，使用处可随意读写用户内存而无安全检查。
2. **泛型函数直接解引用物理地址**：`translated_refmut<T>` / `get_data<T>` / `put_data<T>` 直接将用户 VA 翻译为 PA 后强转 `&mut T`，无对齐检查，无跨页处理（仅 `try_get_data` 有）。

### 修改

删除的函数：
- `translated_byte_buffer`、`safe_translated_byte_buffer`
- `translated_ref`、`translated_refmut`
- `get_data`、`try_get_data`、`put_data`

新增的安全接口：

| 函数 | 用途 |
|------|------|
| `copy_from_user_val<T>` | 从用户空间安全拷贝任意 `T: Sized` 到内核 |
| `copy_to_user_val<T>` | 从内核安全拷贝任意 `T: Sized` 到用户空间 |
| `try_copy_from_user_val<T>` | 可失败版本（futex 场景） |

底层均调用 `copy_from_user` / `copy_to_user`，自动处理跨页、延迟页分配、VA 合法性检查。

新增内部辅助函数（`pub(crate)`，mm crate 内用）：
- `read_user_bytes_direct` / `write_user_bytes_direct`：页表直读（mmap 写回/缺页场景，页面已保证映射）
- `user_buffer_from_kernel`：从内核分配构造 `UserBuffer`（替代 `safe_translated_byte_buffer`）

### 影响范围

共修改 12 个文件，涉及 task、futex、signal、mm、syscall 各模块。所有用 `safe_translated_byte_buffer` 的地方改为内核缓冲区 + `copy_from_user`/`copy_to_user`，消除了零拷贝但不安全的直接页面引用。

---

## 重构 2: 移除 `task.get_fd_table()` 统一锁路径

### 背景

`task.get_fd_table()` 允许不经过 `proc_inner` 锁直接获取 `Arc<FdTable>`，在与其他锁组合时可能导致死锁。新规则：访问进程内部资源必须先获取 `proc_inner = task.process.inner_lock()`。

### 修改

删除 `TaskControlBlock::get_fd_table()` 方法（已被用户提前删除），所有 20+ 处 `task.get_fd_table()` 调用改为 `proc_inner.fd_table`。涉及 `sys_write`、`sys_read`、`sys_writev`、`sys_readv`、`sys_poll`、`sys_pselect6`、`sys_accept4`、`sys_sendto`、`sys_recvfrom`、`sys_epoll_create1`、`sys_inotify_init1`、`sys_eventfd`、`sys_mq_open`、`sys_flock`、`sys_close_range`、`sys_getsockopt`、`sys_setsockopt`、`Socket::from_fd` 等。

对于需要 `fd_table` 在释放锁后继续存活的场景（如 `sys_accept4` 中 accept 可能阻塞），在锁内 `fd_table.clone()` 获取 `Arc`。

---

## 验证

`pthread_robust_detach` 测试通过。所有三个错误路径均已修复：

1. 损坏的 robust list 不再 panic → 优雅停止遍历
2. sigtimedwait 不再污染 `interrupted` 标志 → waitpid 正常阻塞
3. exit_signal 不再被 CLONE_THREAD 覆盖 → waitpid 找到子进程返回 PID

---

## 涉及文件

| 文件 | 修改 |
|------|------|
| `os/src/mm/translate.rs` | 删除旧函数，新增 `copy_from_user_val`/`copy_to_user_val`/`try_copy_from_user_val`；新增内部辅助函数；所有 `VirtAddr::from` → `try_from` |
| `os/src/task/task/task.rs` | `clone_process` 两阶段重构避免死锁；exit_signal 加 `!CLONE_THREAD` 守卫；移除 `token` 变量 |
| `os/src/task/futex.rs` | `try_get_data` → `try_copy_from_user_val`；`put_data` → `copy_to_user_val`；函数签名 `token: usize` → `&MemorySet` |
| `os/src/task/mod.rs` | 调用方适配 `handle_futex_when_exit` 新签名 |
| `os/src/signal/mod.rs` | `get_data`/`put_data` → `copy_from_user_val`/`copy_to_user_val`；`restore_frame` 持有 `memory_set` 而非 `token` |
| `os/src/syscall/signal.rs` | `sigtimedwait` 超时后 `task.clear_interrupt()` |
| `os/src/syscall/task/wait.rs` | 新增 children 状态调试日志 |
| `os/src/syscall/sys.rs` | `safe_translated_byte_buffer` → `copy_from_user`/`copy_to_user` |
| `os/src/syscall/net/io.rs` | `iovecs_to_user_buffer` → `iovecs_to_buf_and_ub`（内核缓冲区） |
| `os/src/syscall/fs/io.rs` | 所有 I/O syscall 去 `get_fd_table`，统一 `proc_inner` |
| `os/src/syscall/fs/mod.rs` | 同上 + `read_user_cstr` 替代 |
| `os/src/syscall/fs/ctl.rs` | 同上 |
| `os/src/syscall/fs/fd_ops.rs` | 同上 |
| `os/src/syscall/fs/event.rs` | 同上 |
| `os/src/syscall/fs/mqueue.rs` | 同上 |
| `os/src/syscall/io_mpx/select.rs` | 同上 |
| `os/src/syscall/io_mpx/poll.rs` | 同上 |
| `os/src/syscall/io_mpx/epoll.rs` | 同上 |
| `os/src/syscall/net/opt.rs` | 同上 |
| `os/src/syscall/net/socket.rs` | 同上 |
| `os/src/syscall/net/cmsg.rs` | 同上 |
| `os/src/mm/memory_set/mmap_ops.rs` | `translated_byte_buffer` → `read_user_bytes_direct` |
| `os/src/mm/page_fault_handler.rs` | 同上 |
| `os/src/mm/memory_set/mod.rs` | 同上 |
| `os/src/fs/files/net.rs` | `Socket::from_fd` 去 `get_fd_table` |
