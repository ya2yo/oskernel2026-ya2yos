# pthread_cancel_points 测试失败：COW 页错误 + CLONE_THREAD a0 非零

### 背景

LTP `pthread_cancel_points` 测试（通过 runtest 启动静态 musl 二进制）创建子线程并测试 pthread cancel 功能。内核在以下两个环节同时存在问题：

1. ELF loader 将跨 `.rodata`/`.data` 段边界的页面映射为 `R|X`，线程初始化写入时触发 StorePageFault，`handle_cow_page_fault` 无法处理
2. `CLONE_THREAD` 分支未按 Linux 惯例将子线程 `a0` 置零，musl `__clone` 汇编无法区分子/父线程

### 现象

**第一次运行（旧内核）：**

```
[PID 5] [TID 6] StorePageFault at stval=0x3efac, sepc=0x3f53c
[lazy_page_fault] valid pte found → 旧版 lazy_page_fault 内部处理成功
[PID 5] [TID 6] FetchInstructionPageFault at stval=0x2a234473d0 → SIGSEGV
```

旧内核的 `lazy_page_fault` 包含"valid pte found"路径可处理非 COW 写错误，但随后子线程取指越界，QEMU 终止。

**第二次运行（当前内核）：**

```
[PID 3] [TID 4] StorePageFault at stval=0x3efac, sepc=0x3f53c
[lazy_page_fault] → 返回 false（PTE 有效但非懒分配）
[cow_page_fault] → handle_cow_page_fault: COW flag 未设置 → 返回 false
→ SIGSEGV → TID 4 死亡 → TID 3 在 futex_wait 死锁
```

`handle_cow_page_fault` 在 commit `731c2a8` 中加入了 `if !COW { return false }` 检查，导致连 StorePageFault 都过不去，TID 4 直接死在第一步。

### 分析

#### 问题 1：handle_cow_page_fault 的 COW 标志检查过严

commit `731c2a8` 的 diff 自述：

```diff
-if !pte_flags.contains(RVPTEFlags::COW) {
-    // 在原来的写法中，不论是否有cow标志都会返回true
-    panic!("ly: a valid pte without COW flag found");
-}
+if !pte_flags.contains(RVPTEFlags::COW) {
+    return false;  // 交给上层按普通用户页错误处理 → SIGSEGV
+}
```

旧版无论 COW 标志是否存在都返回 true（"修复"权限），新版改为返回 false。但 ELF loader 映射 `R|X` 的页面中可能包含 `.data`/`.bss` 的 writable 数据（跨段同页），线程初始化（musl `__pthread_start`）需要写入这些页面。

Linux 的 `do_wp_page` 做法是：不看 COW 标志，检查 PTE 是否只写、VMA 是否允许写，用 `page_mapcount`（等价于 `Arc::strong_count`）决定复用或复制。我们的修复对齐了这一行为。

#### 问题 2：CLONE_THREAD 子线程 a0 非零

`clone_process` 中：

```rust
if flags.contains(CloneFlags::CLONE_THREAD) {
    *child_inner.trap_cx() = *parent_inner.trap_cx();  // a0 = TID（非零）
    // 缺少 child_inner.trap_cx().set_a0(0);
} else {
    child_inner.trap_cx().set_a0(0);  // 仅 fork 分支设零
}
```

musl `__clone`（RISC-V 汇编）：

```asm
    ecall
    beqz a0, 1f    # a0==0 → 子线程 child path
    ret             # a0≠0 → 父线程直接返回
1:  ld a1, 0(sp)   # 加载 arg
    ld a0, 8(sp)   # 加载 fn 指针
    jalr a0         # 跳转到线程入口
```

`a0 = TID ≠ 0` → 子线程走 `ret` 路径，永远无法进入 child path。Linux `copy_thread` 对所有 clone 变体（含 CLONE_THREAD）都强制 `childregs->a0 = 0`。

### 修复

**handle_cow_page_fault（riscv64 + loongarch64）：**

- 移除 `if !COW { return false }` 检查
- 通过 `vma.data_frames.get()` 获取 frame，`Arc::strong_count` 判断引用数
- `refcnt == 1`：直接加 `W + DIRTY`，无需复制
- `refcnt > 1`：unmap + map_one 分配新帧，copy data，更新权限
- `flags.remove(COW)` 在无 COW 时为空操作，对 ELF 页同样正确

**clone_process：**

```rust
if flags.contains(CloneFlags::CLONE_THREAD) {
    *child_inner.trap_cx() = *parent_inner.trap_cx();
    child_inner.trap_cx().set_a0(0);  // 与 Linux 一致
}
```

### 涉及文件

- `os/src/arch/riscv64/qemu/page_table.rs` — `handle_cow_page_fault` 重构
- `os/src/arch/loongarch64/qemu/page_table.rs` — 同上
- `os/src/task/task/task.rs` — CLONE_THREAD `set_a0(0)`

### 验证

待 RISC-V 重跑 `pthread_cancel_points` 验证两处修复联合效果。
