# page fault 统一入口重构

## 背景

Ya2yOS 原先在 trap 层直接按顺序调用 `lazy_page_fault()` 和 `cow_page_fault()`：

```rust
ok = memory_set.lazy_page_fault(fault_va.floor(), cause);
if !ok {
    ok = memory_set.cow_page_fault(fault_va.floor(), cause);
}
```

这套结构能工作，但命名容易造成误解。`lazy_page_fault()` 实际处理的是“PTE 不存在时的按需分配 / mmap 装页”；`cow_page_fault()` 名字像是只处理 COW，但实际承担的是“PTE 已存在但 store 权限不足时的写保护修复”，其中既包括真正的 COW，也包括 ELF / mmap / brk / stack 上可以恢复写权限或复制页面的场景。

## 现象

排查 `pthread_cancel_points` 文档时发现一个表达和代码结构上的问题：valid PTE 仍可能因为缺少写权限触发 `StorePageFault`，随后被当前分发逻辑交给 `cow_page_fault()`。这不是“valid 页面触发 COW”，而是“present PTE 上的写权限 fault 进入写保护修复路径”。

原命名让后续阅读者容易误判：

- PTE valid 为什么还能 page fault；
- 非 COW 页为什么会进入 `handle_cow_page_fault()`；
- trap 层为什么需要知道 lazy 和 COW 的处理顺序。

## 分析

page fault 至少要区分两类场景：

1. **not-present fault**：页表项不存在，需要根据 VMA 类型分配匿名页、装入 mmap 文件页，或为 brk/stack 懒分配物理页。
2. **present permission fault**：页表项存在，但当前访问权限不足。例如 store 到只读 COW 页、LoongArch dirty 位未置位的可写页，或其他可以由 VMA 语义修复的写保护页。

原有 `cow_page_fault()` 实际属于第二类，但函数名只体现了其中一个具体实现细节。trap 层也不应该直接编排“先 lazy 再 cow”，因为这是内存管理子系统内部的 page fault 策略。

## 根因

缺页处理入口按实现细节命名并暴露给 trap 层，导致职责分散：

- trap 层负责异常分发，却知道了内存管理内部的 lazy / COW 顺序；
- `cow_page_fault()` 同时处理 COW 和非 COW 写保护页，名字过窄；
- 架构页表函数 `handle_cow_page_fault()` 的名字不能准确表达 present PTE write fault 修复。

## 修复

本次重构不改变实际页表修复语义，只调整入口和命名：

1. 在 `MemorySet` / `MemorySetInner` 增加统一入口 `handle_page_fault(vpn, scause)`。
2. 将原先的 lazy 分支改为内部函数 `handle_not_present_page_fault()`。
3. 将原先的 COW 外层分支改为内部函数 `handle_write_protect_page_fault()`。
4. 将 `page_fault_handler::cow_page_fault()` 改名为 `write_protect_page_fault()`。
5. 将双架构页表方法 `handle_cow_page_fault()` 改名为 `handle_write_protect_page_fault()`。
6. trap 层和用户指针写入路径统一调用 `memory_set.handle_page_fault()`，不再直接编排 lazy / COW。

重构后语义为：

```text
handle_page_fault()
  ├─ handle_not_present_page_fault()
  └─ handle_write_protect_page_fault()
       └─ PageTable::handle_write_protect_page_fault()
```

其中 `handle_write_protect_page_fault()` 仍保留原有逻辑：根据 PTE、`MapArea` 和 `data_frames` 引用计数决定恢复写权限、设置 dirty，或执行 COW 复制。

## 涉及文件

- `os/src/trap/mod.rs`
- `os/src/mm/translate.rs`
- `os/src/mm/memory_set/mod.rs`
- `os/src/mm/memory_set/mmap_ops.rs`
- `os/src/mm/page_fault_handler.rs`
- `os/src/arch/riscv64/qemu/page_table.rs`
- `os/src/arch/loongarch64/qemu/page_table.rs`

## 验证

已执行：

```text
make
timeout 120s make run > log.ans 2>&1
```

结果：

- `make` 双架构通过。
- `timeout 120s make run` 已运行经过 `basic-musl`、`basic-glibc`、`busybox-musl`、`busybox-glibc`、`lua`、`iperf`、`cyclictest` 和 `libctest-musl` 的部分测试，最终由外层 timeout 终止。
- 日志中未匹配到 `panic`、`StorePageFault`、`LoadPageFault`、`FetchInstructionPageFault`、`PageModifyFault` 或 `SIGSEGV`。
