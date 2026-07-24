# clone procfs 目录延迟物化优化

## 背景

每次非线程 `clone` 都需要发布 `/proc/<pid>`，原实现把该目录立即创建到 EXT4，
而 stat/status/maps/pagemap 内容已经改为按需生成。CAgent CPU 测例包含多次 clone，
因此目录创建仍位于 clone 的同步关键路径。

## 现象

优化前样本中 `clone(samples=11 total_us=231527)`，其中
`clone_duration procfs(samples=11 total_us=95703 max_us=37619)`，地址空间部分仅
`20952 us`。测试功能本身可以通过，但 procfs 元数据操作使 clone 明显偏慢。

## 分析

通用 `open(O_CREATE|O_DIRECTORY)` 会重复执行父目录和子项查找，并进行父目录
`fstat`、权限/umask、mode/owner 更新、EXT4 目录创建及临时 `OSFile` 封装。对 clone
而言，进程目录只需要在用户真正访问 procfs 时存在，提前写入 EXT4 没有必要。

## 根因

procfs 目录的物化时机与文件内容的按需生成不一致：文件内容延迟了，但目录仍在每次
clone 中同步创建，导致每个 clone 都承担 EXT4 全局锁和目录事务开销。

## 修复

- `create_proc_dir()` 只向内存中的 PID 集合登记活跃进程，不再访问 EXT4。
- 首次打开 `/proc/<pid>` 或其 stat/status/maps/pagemap 子路径时，
  `ensure_proc_path()` 调用 `ensure_proc_dir()` 物化目录。
- `getdents64` 枚举 `/proc` 前批量物化活跃 PID，保持目录枚举语义。
- 进程回收时从 PID 集合移除，并保留已有的 proc 文件和目录清理逻辑。
- 物化路径使用 `create_dir_fast()`，父目录 inode 缓存缺失时回退到根 inode 查找。
- `mincore` 对未映射范围返回 `ENOMEM` 属于正常失败结果，地址范围诊断从 error 降为
  debug，避免污染评测日志。

## 涉及文件

- `os/src/fs/kernel_fs_ops/proc_file.rs`
- `os/src/fs/kernel_fs_ops/mod.rs`
- `os/src/fs/mod.rs`
- `os/src/syscall/fs/fd_ops.rs`
- `os/src/syscall/fs/ctl/directory.rs`
- `os/src/fs/vfs.rs`
- `os/src/fs/ext4_lw/inode.rs`
- `os/src/mm/memory_set/accessors.rs`

## 验证

RISC-V `log.ans` 优化后为 `clone(samples=11 total_us=104066 max_us=42382)`，
`clone_duration procfs(samples=11 total_us=231 max_us=109)`，
`clone_duration address_space(samples=11 total_us=16274 max_us=4452)`；
CAgent 输出 `testcase cagent cpu pass 603` 并正常 `shutdown!`。RISC-V 与 LoongArch64
`make perf` 均通过，仅有既有 smoltcp 编译警告。
