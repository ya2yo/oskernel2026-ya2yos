# fork 处理懒分配页时的 COW panic

## 背景

初赛 musl 动态测试在 `pthread_cancel` 之后继续执行时，内核在 RISC-V 页表 COW
处理路径触发 panic；同一逻辑也需要在 LoongArch64 保持一致。

## 现象

日志显示：

```text
src/arch/riscv64/qemu/page_table.rs:548
called `Option::unwrap()` on a `None` value
```

调用路径为 `clone_process()` -> `MemorySetInner::from_existed_user()` ->
`handle_cow_mapping_from_exited_user()`。

## 分析

fork 会遍历 ELF/`brk` VMA 的 VPN 范围并尝试把已有用户页改成 COW。VMA 覆盖范围
可能包含尚未按需分配的页，或已经被解除映射的页；这些 VPN 没有有效叶子 PTE。

## 根因

`find_valid_pte(vpn)` 在缺页时返回 `None`，旧代码无条件 `unwrap()`，把正常的懒分配
状态错误地升级为内核 panic。

## 修复

RISC-V 和 LoongArch64 的 `handle_cow_mapping_from_exited_user()` 改为仅在找到有效
PTE 时执行 COW 标记；缺失叶子直接返回，由子进程后续缺页路径继续按需建立映射。

## 涉及文件

- `os/src/arch/riscv64/qemu/page_table.rs`
- `os/src/arch/loongarch64/qemu/page_table.rs`

## 验证

- `make log TARGET_ARCH=riscv64` 构建通过。
- `make run TARGET_ARCH=riscv64` 的完整 preliminary musl libc 套件通过并输出
  `shutdown!`，无 panic。
- LoongArch64 代码路径已完成对应构建修改，完整 QEMU 测试未在本轮执行。
