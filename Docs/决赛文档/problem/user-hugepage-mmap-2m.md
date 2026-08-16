# 用户态匿名 2 MiB hugepage 映射

## 背景

Ya2yOS 原有用户态 `mmap` 只建立 4 KiB 粒度的延迟分配 VMA，无法处理 Linux
`MAP_HUGETLB` 请求。需要先提供一个可用的最小闭环，支持用户程序申请、访问和释放
大页，供后续 hugepage 测例和更完整的 hugetlb 语义继续扩展。

## 现象

用户程序使用 `mmap(..., MAP_PRIVATE | MAP_ANONYMOUS | MAP_HUGETLB, ...)` 时，内核将
该标志当作未知位或按普通 4 KiB 映射处理，不能保证 2 MiB 虚拟地址对齐，也没有连续
物理页和高级页表叶子，访问请求无法建立正确的 hugepage 映射。

## 分析

Sv39 和 LoongArch64 的三级页表都支持 2 MiB 的 level-1 叶子，但原有映射路径只会
创建末级 4 KiB PTE。物理页则来自 CMA buddy allocator；普通 `FrameTracker` 按单页
布局释放，不能直接用来管理一次按 2 MiB 对齐布局分配的连续块。除此之外，通用 mmap
地址搜索没有对齐约束，`munmap`/`mprotect`/`madvise` 的 VMA 拆分逻辑也不能安全地
拆开一个高级叶子。

## 根因

缺少四个配套边界：`MAP_HUGETLB` 的 ABI 校验、2 MiB 对齐的虚拟地址和 CMA 分配、
RISC-V/LoongArch64 的高级叶子页表操作，以及与连续物理块匹配的生命周期管理。若把
2 MiB 块错误拆成 512 个普通释放记录，最终会以 4 KiB 布局归还 buddy allocator，造成
堆元数据损坏风险。

## 修复

- 在两个 QEMU 架构定义统一的 `HUGE_PAGE_SIZE = 2 MiB` 和页数常量；`mmap` 识别
  `MAP_HUGETLB`，只接受匿名、非零且为 2 MiB 整数倍的请求，支持省略大小编码和
  `MAP_HUGE_2MB`，并拒绝文件后备、其他 huge size 和 `PROT_NONE` 初始映射。
- 增加按页数和对齐粒度调用 CMA buddy allocator 的接口；huge VMA 使用 2 MiB 对齐的
  非固定地址搜索，固定映射要求地址和长度同样按 2 MiB 对齐。
- RISC-V 使用 Sv39 level-1 leaf，LoongArch64 在二级目录项写入 2 MiB leaf；页表
  遍历、地址翻译、用户拷贝和故障诊断都能识别高级叶子并计算页内偏移。
- huge VMA 仍按 4 KiB 保存 `data_frames`，但每组 512 个 `FrameTracker` 共享一个
  `HugeFrameBlock`，最后一个引用按原始 CMA 对齐布局一次性释放连续块。
- huge 映射采用 eager 分配和安装页表。`MAP_SHARED` fork 复用同一组物理帧并重新建立
  huge leaf；`MAP_PRIVATE` fork eager copy 到新的连续 huge block，保持父子隔离。
- 完整 2 MiB 粒度的 `munmap` 可用；非整页拆分、huge `mremap`、部分 huge
  `mprotect`、`MADV_DONTNEED` 和文件/hugetlbfs 后备暂返回 `EINVAL` 或 `EOPNOTSUPP`，
  避免把高级叶子降级成不一致的普通 PTE。

## 验证

- `make TARGET_ARCH=riscv64` 完成 RISC-V64 和 LoongArch64 release 构建；输出仅包含
  仓库已有的 smoltcp/未调用入口 warning。
- RISC-V QEMU 使用 `/tmp` qcow2 overlay 启动，临时用户态 smoke test 成功申请
  2 MiB 对齐地址，读写首尾 `usize`，并完整 `munmap`，输出 `hugepage regression: PASS`。
- LoongArch64 完成 release 编译；尚未运行 LoongArch64 QEMU hugepage smoke test。
