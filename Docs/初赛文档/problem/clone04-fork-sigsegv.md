# clone04: 缺页未发 SIGSEGV 与 _Fork 语义

### 背景

LTP `clone04` 测试 `clone(0, child_fn, NULL, CHILD_STACK_SIZE, NULL)` 应返回 EINVAL（NULL 栈非法）。实际运行时 musl clone wrapper 在发系统调用前向 `NULL-16` 写入触发 StorePageFault，内核未正确发 SIGSEGV 导致进程异常终止（退出码 254）。

### 现象

```
clone04 : 254
Exception(StorePageFault) in application, bad addr = 0xfffffffffffffff0
don't send SIGSEGV, just exit the process
```

### 故障链

```
musl __clone wrapper:
    sp = child_stack - 16    →  NULL - 16 = 0xfffffffffffffff0
    *(sp + 0) = fn           →  StorePageFault

内核 trap handler:
    lazy_page_fault  → false （无 VMA 覆盖该地址）
    cow_page_fault   → false
    has_sigsegv_handler? → false （无自定义 handler）
    → exit_current_and_run_next(-2)  // 绕过信号机制，直接 exit
```

### 分析

**根因 1 — 缺页异常处理逻辑有缺陷（trap/mod.rs）：**

原代码只在进程注册了自定义 SIGSEGV handler 时才发送信号，否则直接 `exit(-2)`。正确的行为应是**始终发送 SIGSEGV**——无论有无自定义 handler，由信号分发机制根据默认动作（terminate）或自定义 handler 处理。

**根因 2 — fork 路径子进程 sig_mask 继承自父进程（task.rs）：**

`_Fork` 语义要求子进程信号掩码干净，所有信号不被阻塞，确保 AS-safe 函数正确工作。原代码 `sig_mask = parent_inner.sig_mask` 继承了父进程可能的部分阻塞掩码。

### 修复

**trap/mod.rs：** 删掉 `has_sigsegv_handler` 条件分支，缺页无法恢复时无条件发送 SIGSEGV：

```rust
if !ok {
    let tid = current_task().unwrap().tid();
    send_signal_to_thread(tid, SigSet::SIGSEGV);
    return;
}
```

之后 `trap_return` 检测到 pending SIGSEGV → `handle_signal(SIGSEGV)` → 无自定义 handler → 默认动作 → `exit_current_and_run_next(128 + 11 = 139)`。

**task/task/task.rs：** fork 路径子进程 sig_mask 改为 `SigSet::empty()`：

```rust
sig_mask = SigSet::empty();  // 原为 parent_inner.sig_mask
```

与 robust_list 默认空、sig_pending 默认空一起，构成完整 _Fork 清理语义。

### 涉及文件

- `os/src/trap/mod.rs` — 缺页异常直接发 SIGSEGV
- `os/src/task/task/task.rs` — fork 路径 sig_mask 清零

### 验证

RISC-V 单跑 clone04：进程被 SIGSEGV 正确终止（退出码 139 替代原 254）。
