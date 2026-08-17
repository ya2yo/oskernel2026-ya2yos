# brk 与 MAP_FIXED VMA 重叠边界

## 现象

`brk()` 使用延迟分配，只扩大 `MapAreaType::Brk` 的 VPN 范围。普通 `mmap()` 会从 `MMAP_TOP` 向下寻找空闲区域，因此通常不会覆盖 brk；但 `MAP_FIXED` 可以指定 brk 保留区内的地址。

旧实现的 `MAP_FIXED` 清理路径调用只处理 mmap VMA 的 `munmap()`，不能截断 brk 区域。若固定映射落在当前 brk 范围或未来扩展范围内，后续 brk 扩展就可能留下重叠 VMA。缺页处理按 area 顺序查找，重叠条目的顺序会影响最终权限和缺页语义。

## 修复

- `MAP_FIXED` 覆盖 brk 时先调用 `remove_brk_range()`，按页解除被覆盖的 brk VMA，保留未覆盖的左右片段，再插入固定映射；不再留下重叠 VMA。
- `TaskInner` 继续单独保存逻辑 brk 指针。`MemorySetInner::grow()` 扩大 brk 时只在目标范围的空闲子区间建立 `Brk` VMA，因此允许形成 `brk | mmap | brk`；收缩时只移除 brk 尾部。
- `sys_brk()` 分别计算向上和向下的地址差值，避免 `usize` 减法下溢；`grow()` 用 `Option` 返回地址溢出、超出上限或 VMA 冲突，失败时不更新进程 brk 指针。

## 验证

```text
make TARGET_ARCH=riscv64 build-arch
make TARGET_ARCH=loongarch64 build-arch
```

双架构构建通过。LoongArch64 `mmapstress03` 输出 `TPASS: Test passed` 并返回 0；固定映射覆盖 brk 后的后续扩展不再出现重叠 VMA。
