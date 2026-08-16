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

## LTP huge 测例验证

为复用现有 LTP 测例，测试期间临时将 `scripts/riscv64.mk` 的 `DISK_IMG` 切换到
`2026_testsuits_img/pre_tests/sdcard-rv.img`，并让 `initproc::test_pre()` 临时调用现有的
`ltp::test_glibc_single()` 入口（先验证 `hugemmap06`，再按 blacklist 扩展）；测试完成后已
恢复决赛镜像和原始 `initproc`。
同时为满足 LTP 的 hugepage 预置查询，临时创建了 fake `/proc/sys/vm` 和 `/sys` 文件，
随后全部删除，`/proc/meminfo` 的 `HugePages_Total/Free` 也恢复为 0，这些接口不属于正式
实现。

按 `user/src/bin/ltp/blacklist.rs` 逐项调用 31 个 `hugemmap*` 入口后，只有匿名
`MAP_PRIVATE | MAP_ANONYMOUS | MAP_HUGETLB` 的 `hugemmap06` 真正进入映射逻辑，最终输出
5 项 `TPASS`，汇总为 `passed 5 failed 0 broken 0` 并正常 `shutdown!`。测试过程中
`MAP_FIXED` 重新映射曾触发页表断言：旧的 4 KiB 子页表已清空但 level-1 中间项仍为
`VALID`。`os/src/arch/riscv64/qemu/page_table.rs` 现仅在确认下级 512 项全部无效时回收
该中间项，若仍有活动映射则继续拒绝覆盖；修复后重复运行 `hugemmap06` 无 panic。

其余 `hugemmap01/02/04/07/09/11-31` 都是文件后备 `hugetlbfs`，在挂载阶段报告
`TBROK: ... mount ... ENODEV`；`hugemmap05/08/10` 因未实现
`nr_overcommit_hugepages` 报 `TCONF`，`hugemmap32` 因 gigantic hugepages 报 `TCONF`。
另行运行的 `futex_wake04` 因静态 proc 未提供 `/proc/<pid>/task` 报 `TBROK`。这些结果
说明 blacklist 的其余条目确实超出当前匿名 2 MiB 最小实现范围，并非匿名映射回归失败。

继续复用 blacklist 中的 `hugefallocate*`、`hugefork*` 和 `hugeshm*` 后，前两类同样在
`hugetlbfs` 挂载阶段为 `TBROK/ENODEV`，`hugeshmat01-03` 在读取缺失的
`/proc/sys/kernel/shmmax` 时为 `TBROK`，`hugeshmat04` 因内存条件为 `TCONF`。其中
`hugeshmat05` 曾暴露 SysV shm 标志解析的 `unwrap()` panic；`os/src/syscall/mm/shm.rs`
现对未知标志返回 `EINVAL`，复测结果为 `shmget failed: EINVAL`，内核不再崩溃。SysV
huge shm 仍不属于本次匿名映射实现范围。

## 用户态定向回归测例

新增 `user/src/bin/initproc/hugepage_regression.rs`，按现有
`fstat_unlink_regression.rs` 的结构实现跨架构原始 syscall 回归，并在 `initproc::test()`
中顺序调用。测例检查非 2 MiB 整数倍长度被拒绝、返回地址按 2 MiB 对齐、两个连续大页的
首尾读写、`MAP_FIXED` 重映射后第二页数据保持不变，以及两个 2 MiB 区间分别 `munmap`。

RISC-V64 QEMU 定向运行输出 `hugepage regression: PASS` 并正常 `shutdown!`；恢复原始
`main()` 后，RISC-V64 和 LoongArch64 release 构建均通过。该测例只接入已有定向 `test()`
入口，不改变正式镜像的 `run_selected_tests()` 启动路径。
