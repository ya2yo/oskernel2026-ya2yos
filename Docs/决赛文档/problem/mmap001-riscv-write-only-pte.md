# LTP mmap001 RISC-V PROT_WRITE PTE 编码卡死修复

## 背景

LTP `mmap001` 创建一个 1000 页（4,096,000 字节）的普通文件，以
`MAP_SHARED | PROT_WRITE` 建立映射，逐字节写入整个映射区，再执行 `msync()` 和
`munmap()`。该用例覆盖文件共享映射的首次写缺页、页缓存映射和同步回写路径。

## 现象

原始 `log.ans` 中，`mmap()` 已成功返回：

```text
[sysmap] addr=0x0,len=4096000,prot=0x2,flags=0x1,fd=3,off=0
[mmap] addr=0, len=4096000, map_perm=W | U, flags=MAP_SHARED
[sys_mmap] alloc addr=0x2a2305f000
```

测试随后第一次写入返回地址 `0x2a2305f000` 时，不再产生 syscall 或 LTP 结果，而是反复
打印同一条：

```text
[handle_write_protect_page_fault] va=VA:0x2a2305f000
```

日志没有 `TPASS`、`TFAIL`、`TBROK`、Summary 或 panic，说明同一条用户态 store 指令持续
陷入页异常后被原样重试。

## 分析

`PROT_WRITE` 在 syscall 层正确保留为 VMA 的逻辑权限 `MapPermission::W | U`。首次 store
fault 命中 `MAP_SHARED` 文件页缓存后，`handle_mmap_read_page_fault()` 会通过
`RVPTEFlags::from(vma.map_perm)` 安装硬件 PTE。

旧转换将该权限编码为 `V|W|U`，没有设置 `R`。RISC-V 规定叶 PTE 的 `R=0,W=1` 是保留
组合，不能作为可写映射使用。内核的软件页表查询仍把该 PTE 视作 present，因此不会回到
not-present 的懒分配路径；随后写保护处理发现 PTE 已有 `WRITEABLE`，只补 `DIRTY`、刷新
TLB 并返回成功。PTE 仍为非法 `R=0,W=1`，原 store 重试后便再次 fault，形成无限循环。

这不是 COW 或 TLB 刷新遗漏：该映射是 `MAP_SHARED`，没有走 COW 分裂分支；循环路径已经
执行 TLB invalidation。Linux RISC-V 也在 `riscv_sys_mmap()` 中将 write-only protection
规范化为可读可写硬件保护，且 `PAGE_WRITE` 包含 `_PAGE_READ | _PAGE_WRITE`。

## 根因

RISC-V 的 `MapPermission -> RVPTEFlags` 转换将逻辑 write-only VMA 直接映射为硬件
write-only PTE，违反架构 `W => R` 约束。`mprotect()` 的临时权限位构造也绕过该转换，存在
重建同一非法编码的旁路。

## 修复

- 在 `RVPTEFlags::from(MapPermission)` 中，当 VMA 具有 `W` 时同时设置硬件
  `READABLE` 和 `WRITEABLE`。`MapPermission` 本身保持 `W`，因此内核的 VMA/user-copy
  权限检查仍以原始逻辑权限为准。
- `PageTable::handle_mprotect()` 对请求的硬件权限做同样的 `W => R` 规范化。该路径继续
  使用不含 `VALID` 的临时 flags，避免把懒分配 PTE 的 `PPN=0` 条目错误变成有效映射；原有
  的权限合并语义没有在本次修改中扩展。

## 涉及文件

| 文件 | 修改 |
|---|---|
| `os/src/arch/riscv64/qemu/page_table.rs` | 将 RISC-V 可写 PTE 规范化为 `R|W`，并覆盖 mprotect 的直接权限位路径 |

## 验证

已执行：

```text
cargo fmt --manifest-path os/Cargo.toml -- --check
make TARGET_ARCH=riscv64
make log TARGET_ARCH=riscv64
timeout 120s make run TARGET_ARCH=riscv64 > log.ans 2>&1
make build-arch TARGET_ARCH=loongarch64
git diff --check
```

`make TARGET_ARCH=riscv64` 命中根目录默认 `all`，完成 RISC-V 和 LoongArch64 release
构建；随后显式 LoongArch64 release 构建再次通过。两次构建仅有既有 Cargo config 弃用提示
和 vendored `smoltcp` warnings。

最终 RISC-V QEMU 输出直接保存在根目录 `log.ans`，其中包含：

```text
mmap001     1  TPASS  :  mmap() completed successfully.
mmap001     2  TPASS  :  we're still here, mmaped area must be good
mmap001     3  TPASS  :  synchronizing mmapped page passed
Summary:
passed   4
failed   0
broken   0
#### OS COMP TEST GROUP END ltp-musl ####
shutdown!
```

没有再次出现测试映射首地址的重复 `handle_write_protect_page_fault`。LoongArch64 的 PTE
格式允许独立表达读写权限，本次未修改该架构；已完成编译验证，未单独运行其 mmap001 行为
回归。
