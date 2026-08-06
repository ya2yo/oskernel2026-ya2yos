# brk 与 MAP_FIXED VMA 重叠边界

## 现象

`brk()` 使用延迟分配，只扩大 `MapAreaType::Brk` 的 VPN 范围。普通 `mmap()` 会从 `MMAP_TOP` 向下寻找空闲区域，因此通常不会覆盖 brk；但 `MAP_FIXED` 可以指定 brk 保留区内的地址。

旧实现的 `MAP_FIXED` 清理路径调用只处理 mmap VMA 的 `munmap()`，不能截断 brk 区域。若固定映射落在当前 brk 范围或未来扩展范围内，后续 brk 扩展就可能留下重叠 VMA。缺页处理按 area 顺序查找，重叠条目的顺序会影响最终权限和缺页语义。

## 修复

- `MemorySetInner::grow()` 在扩大 brk 前检查目标 `[heap_bottom, new_brk)` 与其他 VMA 是否重叠。
- `MAP_FIXED` 拒绝覆盖 brk，不再让 mmap 替换路径破坏 brk 的独立边界状态。
- `sys_brk()` 分别计算向上和向下的地址差值，避免 `usize` 减法下溢。
- `grow()` 用 `Option` 返回地址溢出、超出上限或 VMA 冲突，失败时不更新进程 brk 指针。

## 验证

```text
make TARGET_ARCH=riscv64
```

通过。该命令同时完成 RISC-V 与 LoongArch64 的构建，两个架构均成功。全仓 `cargo fmt --all -- --check` 受工作区既有格式差异影响未通过，未格式化无关文件。
