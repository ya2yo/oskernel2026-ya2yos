# mmap LTP 并发、地址 hint 与 memfd 映射语义修复

## 背景

`2.ans` 中的 `mmap1`、`mmap16`、`mmapstress03` 和 `memfd_create01` 在 mmap 相关路径失败。
这些测例分别覆盖多线程缺页与 VMA 替换、非固定地址 hint、`MAP_FIXED` 与 brk 的交互，以及
memfd 文件映射和 sealing/fallocate 语义。

## 现象

- `mmap1` 在大量并发映射/解除映射后报告 `got sigsegv while mapped`。
- `mmap16` 的原位 `mremap` 返回 `ENOMEM`，随后 checkpoint 超时。
- `mmapstress03` 的固定映射操作无法覆盖 brk 保留区。
- `memfd_create01` 在 write-seal 场景中执行 `mmap(PROT_READ)` 时返回 `EINVAL`，后续还暴露
  procfd reopen、seal、fallocate 和独立文件 offset 语义缺失。

## 分析

`mmap1` 的文件缺页页缓存读取在 `MemorySet` 锁外进行，VMA 可能在读取期间被
`munmap()`/`MAP_FIXED` 替换；如果仍按旧 VMA 安装页，用户态会看到错误内容或错误信号。
同时，多 Hart 页表更新需要在安装新 PTE 或替换 COW 物理页后完成远端 TLB shootdown；一次性
present-PTE retry 标记若在信号 handler 通过 `longjmp` 返回后残留，还会污染后续复用同一 VPN
的映射。

`mmap16` 先释放一个地址洞，再用同一非零地址作为普通 mmap hint。旧实现忽略 hint，导致新映射
落在其他位置，原位扩容找不到连续区间。`MAP_FIXED` 与 brk 的旧实现只清理 mmap VMA，不能
拆分逻辑 brk 区间。

`memfd_create()` 返回的是匿名文件对象，而 `sys_mmap()` 原先只识别普通 inode 文件和
`/dev/zero`，因此 memfd 映射直接返回 `EINVAL`。匿名文件还需要共享 backing data/seals，
但每个 reopen fd 应有独立 offset；`fallocate()` 的扩展、保留大小和 hole-punch 也必须受
相应 seal 约束。

## 根因

1. 文件页准备结果没有携带 VMA identity，页表安装缺少并发替换后的重新确认。
2. present-PTE 和 COW PTE 更新没有统一的远端 TLB 可见性协议，信号退出路径也未清理 retry 状态。
3. 普通 mmap 的非零 hint 未参与地址选择；brk 的逻辑范围与实际 VMA 没有分离。
4. memfd 未接入 mmap 文件后备抽象，procfd reopen、seals、fallocate 和 open-file offset 语义不完整。

## 修复

- `MemorySet::handle_page_fault()` 在锁外预取文件页并记录 inode/page index，安装前后校验 VMA
  identity；PTE 安装和 COW 替换通过 `UPDATE_LOCK` 与 remote TLB shootdown 同步，返回用户态前
  对错过广播的 Hart 刷新本地 TLB；同步发送 `SIGSEGV`/`SIGBUS` 前清理 present-PTE retry 标记。
- 非固定 mmap 在 hint 页对齐、完整范围空闲时优先使用 hint；`MAP_FIXED` 覆盖 brk 时拆分并释放
  brk VMA，brk 扩展只在空闲子范围物化 VMA。
- `MmapFile` 持有可选的 `MmapLease`；memfd 共享 backing data、seals 和映射生命周期，支持
  只读 procfd reopen 的权限检查、独立 offset、`O_TRUNC`、seal 拒绝规则和 `fallocate()`。

## 涉及文件

- `os/src/mm/memory_set/{area_ops.rs,handle.rs,mmap_ops.rs}`
- `os/src/mm/map_area.rs`
- `os/src/syscall/mm/mmap.rs`
- `os/src/trap/mod.rs`
- `os/src/fs/{vfs.rs,files/tmp_file.rs}`
- `os/src/syscall/fs/{fd_ops.rs,fcntl.rs,memfd.rs,space.rs}`

## 验证

- `cargo fmt --manifest-path os/Cargo.toml --check`
- `cargo fmt --manifest-path user/Cargo.toml --check`
- `make TARGET_ARCH=loongarch64 build-arch`
- `make TARGET_ARCH=riscv64 build-arch`
- LoongArch64 定向日志：`mmap1` 为 `passed 1 failed 0 broken 0`，`mmap16` 和
  `mmapstress03` 返回 0，`memfd_create01` 为 `passed 157 failed 0 broken 0`。

`mmap3` 的 `TBROK` 来自测试镜像缺少 `libgcc_s.so.1`，导致 glibc `pthread_exit()` 自行
`SIGIOT/SIGABRT`；`io_uring01` 仍属于尚未完成的独立 io_uring 功能，不纳入本次修复结论。
