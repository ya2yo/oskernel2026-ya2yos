# BuildStorm RISC-V rustc SIGSEGV 与 VMA 空洞搜索回归

## 背景

维护者提供的 `server.ans` 来自 `b913068c32d198e292c9373320084e217e83ec17` 之后的工作区。该基线可持续运行约 75 分钟；本次排查不早于该提交。工作区曾有未提交的 rseq 事件驱动优化，因此先进行了回退对照。

## 现象

`sigaltstack regression` 和 `rseq regression` 均通过后，BuildStorm 在 `440/446: ax-cpumask, ax-memory-addr` 报告：

```text
error: rustc interrupted by SIGSEGV
[0xffffffc080202000]
error: could not compile `ax-cpumask`
```

该地址是 RISC-V 内核 `sigreturn_trampoline` 的起始地址。日志没有 Ya2yOS panic、LTP `TFAIL` 或 `TBROK`；`dep-graph.bin` 零长度 warning 是伴随现象。

## 分析

将未提交的 rseq event-driven/batch-uaccess runtime 改回当前 `HEAD` 行为后，失败仍在同一位置重现，因此它不是本次回归的充分原因。

`f9dc0703` 引入了按起始 VPN 排序/合并 VMA，并把 `find_insert_addr()` 改为反向线性空洞搜索。原实现每次遇到占用区域后，将下一次候选终点移动到该区域起点的前一页；新实现却直接使用 `area_start`。这改变了原有的保护页边界，BuildStorm 的高频 `mmap`、`munmap`、`mremap`、缺页和 `MAP_GROWSDOWN` 生命周期会触发错误的地址选择，最终表现为 rustc 在信号返回 trampoline 附近 SIGSEGV。

分量对照结果如下：

- 保留排序/合并、恢复旧递归搜索：运行推进到 `444/446`，无 SIGSEGV；
- 保留新反向搜索、使用旧的追加式 VMA 容器：启动阶段即无法继续 BuildStorm，说明该搜索依赖排序不变量；
- 保留排序/合并并修正为碰撞后至少下移一页：运行推进到 `444/446`，无 SIGSEGV。

## 根因

`f9dc0703` 的新空洞搜索丢失了旧实现的“一页下移”边界。候选区间直接贴着障碍 VMA 结束，破坏了 `MAP_GROWSDOWN` 所依赖的保护页间隔，并在多次 mmap 拆分后产生错误的用户地址布局。

## 修复

- 保留 `f9dc0703` 的排序插入、匿名私有 VMA 合并和反向线性扫描，避免恢复原先的递归全量重扫。
- 发生碰撞时使用 `area_start - 1` 作为候选终点，并对页号减法做 checked arithmetic；没有可表示的下方页时返回 0。
- 保留 `4bfe9279` 的 present-PTE 缺页快路径和 `a1e84f5a` 的 TCB 生命周期修复。

涉及文件：`os/src/mm/memory_set/area_ops.rs`。

## 验证

- `make TARGET_ARCH=riscv64`：RISC-V 与 LoongArch64 release 构建通过；仅有 vendored `smoltcp` 的既有 unused warning。
- `timeout 180s make run TARGET_ARCH=riscv64`（修正后的搜索）：`sigaltstack regression: PASS`、`rseq regression: PASS`、toolchain/minibuild 通过，BuildStorm 从 `440/446` 推进到 `444/446`；无 SIGSEGV、panic 或 Cargo error。超时发生在完整测试组结束前，不能宣称 `446/446` 或 75 分钟稳定性已验证。

## 后续复核：mremap VMA 拆分索引回归

### 现象

源码修正空洞搜索后，继续审计 `f9dc0703` 对所有 `MapArea` 插入点的改动，发现
`mremap_maymove()` 和 `mremap_in_place()` 的前段/尾段拆分仍使用会排序并合并相邻匿名私有
VMA 的插入路径。该路径可能让 mremap 操作继续使用失效的 `old_idx`，并把尾部阻塞区域重新
合并回源 VMA；这与 `tg-xtask` 编译期间高频 `mmap`/`mremap` 生命周期不兼容。

### 根因

`f9dc0703` 将原先的 `Vec::push()` 统一替换为排序插入和匿名私有 VMA 合并。mremap 在
操作尚未提交时需要同时保留前段、精确源段和尾段，但相邻段具有相同属性，自动合并会撤销
拆分；前段按起始 VPN 插入到源段之前还会使 `old_idx` 增加一位。后续按旧索引访问可能复制
错误帧、检查错误的 VMA，或错误放行原地扩展。

### 修复

- 在 `area_ops.rs` 增加 `insert_area_sorted_unmerged()`，只维护按起始 VPN 排序，不执行合并。
- mremap 的前段和尾段拆分改用该中间态插入接口；前段插入后同步修正 `old_idx`。
- 普通 mmap、ELF、fork 和最终提交路径继续使用排序加兼容 VMA 合并。

涉及文件：`os/src/mm/memory_set/area_ops.rs`、`os/src/mm/memory_set/mmap_ops.rs`。

### 验证

- `make build-arch TARGET_ARCH=riscv64`：通过。
- `make build-arch TARGET_ARCH=loongarch64`：通过。
- `git diff --check`：通过。
- 未重复运行完整 QEMU/BuildStorm；`server.ans` 已提供触发现场，完整 `tg-xtask` 编译窗口较长。
## P0/P1 实现补充（2026-08-07）

在上述排序和碰撞边界修复基础上，P0 增加 `MemorySetInner::mmap_hint`：普通 `mmap`、动态
共享内存和非固定 `mremap` 优先从上一次成功位置向下分配，固定映射、回收高地址区间或
hint 失效时仍回退到有序 VMA 的完整 top-down 查找。`munmap` 会把新释放的高地址空洞重新
暴露给下一次分配；hint 不参与地址合法性判断，因此不改变 `MAP_FIXED`、拆分或增长语义。

P1 在任务内增加 `rseq_pending`。实际上下文切出、注册、exec 和信号返回路径设置或强制消费
该状态；没有事件的普通 syscall 返回跳过 rseq 用户内存访问。CPU ID 与 node/mm 字段改为
两个连续的小段复制，坏指针仍沿用原 EFAULT/SIGSEGV 路径。`rseq_regression` 已改为同时
覆盖“无调度 syscall 不清理”和“阻塞切换后清理”两种语义。

本次仅完成静态检查和双架构 release 构建；按维护者要求未启动 QEMU/BuildStorm 运行验证。
