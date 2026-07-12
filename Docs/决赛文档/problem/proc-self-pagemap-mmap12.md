# LTP mmap12 `/proc/self/pagemap` 缺失修复

## 背景

LTP `mmap12` 将文件以 `MAP_PRIVATE | MAP_POPULATE` 映射后，打开
`/proc/self/pagemap`，定位到 `virtual_address / page_size * sizeof(u64)`，并逐页读取
Linux pagemap 条目。该接口要求每个虚拟页都对应一个二进制 `u64` 条目，而不是
`/proc/<pid>/maps` 那样的文本 VMA 列表。

## 现象

内核已有 `/proc/<pid>/stat`、`status`、`maps` 的创建和刷新逻辑，但缺少
`/proc/<pid>/pagemap`，`sys_openat()` 也没有将 `/proc/self/pagemap` 重写为当前进程
PID 对应的路径。因此访问该文件会在 VFS 查找阶段失败，LTP 无法继续读取 pagemap。

## 分析

Ya2yOS 将 proc 文件实现为普通 VFS 文件，故不能像 Linux procfs 一样在 read 回调中
即时生成数据；现有 `maps` 的做法是在每次 `openat()` 前刷新文件内容。`pagemap`
应沿用该生命周期，但文件偏移的语义不同：偏移 `vpn * 8` 必须读取该页的 `u64`。

页表的 `translate(vpn)` 可返回已建立 PTE 的物理页号。当前内核没有 swap、soft-dirty、
userfaultfd write-protect 或 page exclusivity 状态，因此最小兼容条目只需要设置 bit 63
的 present 标志和 bits 0-54 的 PFN；未映射或尚未缺页分配的页读取为零。

若把用户高地址空间的全部零条目显式写入文件，会为每个进程产生很大的无意义 I/O。
因此文件先按最高 VMA VPN `truncate` 到所需逻辑长度，只对连续 present 页执行
`write_at()`；ext4 稀疏洞在读取时自然返回零。

## 根因

1. 进程 proc 目录创建和退出清理列表中没有 `pagemap`。
2. `/proc/self` 仅为已有的 `stat`、`maps`、`status` 提供了路径重写，未覆盖 `pagemap`。
3. 没有将进程页表转换为 Linux-compatible pagemap 二进制条目的刷新函数。

## 修复

- 在 `proc_file.rs` 新增 `refresh_proc_pagemap()`：在 `MemorySet` 读锁内快照 present
  PTE，释放地址空间锁后再创建、截断并写入 VFS 文件，避免跨 VFS 操作持有地址空间锁。
- `create_proc_dir_and_file()` 创建 `/proc/<pid>` 时刷新 pagemap；进程退出时同时 unlink
  该文件并清理 `FsIndex` 索引；initproc 也显式创建 `/proc/1` 目录及文件。
- `sys_openat()` 将 `/proc/self/pagemap` 重写为 `/proc/<current-pid>/pagemap`，并对直接
  访问 `/proc/<pid>/pagemap` 与 self 路径在打开前刷新当前页表快照。
- 为 `refresh_proc_pagemap()` 补充锁边界、条目编码、连续页合并和稀疏文件写入的注释。

## 涉及文件

| 文件 | 修改 |
|------|------|
| `os/src/fs/kernel_fs_ops/proc_file.rs` | 创建、刷新、稀疏写入和退出清理 `/proc/<pid>/pagemap` |
| `os/src/syscall/fs/fd_ops.rs` | 解析 `/proc/self/pagemap`，并在 open 前刷新 pagemap |
| `os/src/task/task/task.rs` | 为 initproc 创建 `/proc/1` 下的进程文件 |
| `os/src/fs/{mod.rs,kernel_fs_ops/mod.rs}` | 导出 pagemap 刷新接口 |

## 验证

已执行：

```text
make
```

默认 LoongArch64 构建通过，仅有 vendored `smoltcp` 既有 warning。

最新 `log.ans` 的 LoongArch64 单跑 `mmap12` 中，musl 和 glibc 两轮均输出：

```text
mmap12.c:114: TPASS: File mapped properly
Summary:
passed   1
failed   0
broken   0
skipped  0
warnings 0
```

日志中同时有 `The Nth page ... is not present` 的 `TINFO` 行。这是该用例对未设置
present bit 的信息性输出，不改变最终 `TPASS` 与 Summary；本轮验证确认
`/proc/self/pagemap` 的打开、按 `vpn * 8` 定位和 8 字节二进制读取均已走通。

未运行 RISC-V QEMU；本轮没有改动架构专属页表接口，RISC-V 仍需后续回归确认。
