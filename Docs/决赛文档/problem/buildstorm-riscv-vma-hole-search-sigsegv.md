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
