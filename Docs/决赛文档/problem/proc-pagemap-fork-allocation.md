# `/proc/pagemap` 截断导致 fork 停滞

## 背景

为兼容 LTP `mmap12`，内核此前将 `/proc/<pid>/pagemap` 实现为 ext4 上的普通文件：按
最高 VMA 的虚拟页号计算逻辑大小，只写 present PTE 的 `u64` 条目，未映射页依赖文件洞返回
零值。

## 现象

LoongArch64 启动测试缩小到单个 `glibc/basic_testcode.sh` 后，`loongarch.ans` 显示：

1. PID 2 的 `sys_execve("busybox", ["busybox", "sh", "basic_testcode.sh"])` 已返回 `0`；
2. glibc BusyBox 读取脚本并调用 `clone()` 创建 PID 3；
3. PID 3 在创建 `/proc/3/pagemap` 时停在 `file_truncate to 402653184`，不再进入第一个 basic
   子程序。

将 `os/src/syscall/task/execve.rs` 恢复到提交 `456c45e` 前的参数处理后，现象完全不变，排除
了本次停滞由该 `execve` 修改引起的可能。

## 分析

LoongArch 用户 ELF 与栈 VMA 位于较高虚拟地址，`highest_vpn * sizeof(u64)` 为
`402653184` 字节。`create_proc_dir_and_file()` 对每个新进程都调用
`refresh_proc_pagemap()`，后者对 ext4 普通文件调用 `truncate(file_size)`。

当前 lwext4 路径不具备此处所假设的廉价稀疏扩容语义，384 MiB 的截断会产生实际的长时间
分配/写零操作。fork 一个 shell 后再 fork basic 子程序时，第二次大截断就使启动看似停在
BusyBox `execve` 参数打印处。

## 根因

将 Linux 的动态 procfs `pagemap` 用普通 ext4 文件模拟时，错误地把高地址虚拟页索引转化为
每个进程创建时的实际文件扩容；既不属于 `execve`，也不应出现在 `fork` 热路径。

## 修复

- 新增 `PagemapFile` 只读动态文件对象，持有目标地址空间的 `Arc<MemorySet>`，在 `read()` 时
  按当前 offset 生成 Linux pagemap `u64` 条目：present 页输出 bit 63 与 PFN，洞输出零。
- `fstat()` 与 `lseek(SEEK_END)` 仍以最高 VMA 表示 Linux 可见逻辑长度，但不在 ext4 分配对应
  数据块；支持按 `vpn * 8` 的随机读取。
- `sys_openat()` 解析 `/proc/self/pagemap` 或 `/proc/<pid>/pagemap` 后直接返回该动态文件对象，
  并拒绝写打开；`O_CREAT|O_EXCL` 返回 `EEXIST`。
- 进程创建仅留下零大小的 `/proc/<pid>/pagemap` 目录项供目录枚举，删除了创建和打开路径上的
  `refresh_proc_pagemap()` / 大文件 `truncate()`。

## 涉及文件

| 文件 | 修改 |
|------|------|
| `os/src/fs/files/pagemap.rs` | 新增按需生成 pagemap 条目、seek、stat 的抽象文件实现 |
| `os/src/fs/files/mod.rs` | 导出 `PagemapFile` |
| `os/src/syscall/fs/fd_ops.rs` | `/proc/<pid>/pagemap` 打开时创建动态文件描述符 |
| `os/src/fs/kernel_fs_ops/proc_file.rs` | proc 创建仅创建空目录项，移除大文件刷新/截断 |
| `os/src/fs/{mod.rs,kernel_fs_ops/mod.rs}` | 移除旧刷新函数导出 |

## 验证

已执行：

```text
make
make TARGET_ARCH=riscv64
timeout 120s make run
timeout 180s make run
```

- LoongArch64 `glibc/basic_testcode.sh` 完整输出
  `#### OS COMP TEST GROUP END basic-glibc ####` 和 `shutdown!`；没有新的 panic、`TFAIL`、
  `TBROK`，也未再出现 `file_truncate to 402653184`。
- 临时追加的 LoongArch64 glibc `mmap12` 输出 `TPASS: File mapped properly`，Summary 为
  `passed 1 failed 0 broken 0 skipped 0 warnings 0`，确认动态 pagemap 的 open/lseek/read
  兼容路径可用；临时测试调用已删除，`initproc` 保持维护者设置的单个 basic 回归范围。
- LoongArch64 和 RISC-V 构建均通过，仅有 vendored `smoltcp` 既有 warning。

未运行本次改动后的 RISC-V QEMU；该动态文件不含架构专属逻辑，仍建议后续补跑
`mmap12` 回归。
