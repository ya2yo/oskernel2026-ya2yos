# getrusage03: ru_maxrss / RUSAGE_CHILDREN / proc status

### 背景

LTP `getrusage03` 验证 `getrusage(2)` 的 `ru_maxrss` 行为：

- `RUSAGE_SELF`：当前进程最大 RSS
- `RUSAGE_CHILDREN`：已 wait 子进程中的最大 RSS
- 子进程 wait 孙进程后，其资源用量应继续向父进程传递

### 现象

`log.ans` 中主要失败如下：

```text
getrusage03.h:25: TBROK: Failed to open FILE '/proc/self/status' for reading: ENOENT (2)
getrusage03.c:59: TFAIL: initial.children = 0, expected 102400
StorePageFault ... sending SIGSEGV
getrusage03.c:86: TFAIL: child.children = 0, expected 307200
getrusage03.c:101: TBROK: Failed to open FILE '/proc/5/stat' for reading: ENOENT (2)
```

第一轮修复后 `RUSAGE_CHILDREN` 已通过，但仍有：

```text
getrusage03.h:25: TBROK: Expected 1 conversions got 0 FILE '/proc/self/status'
```

从测试镜像中导出 `getrusage03` 后用 `strings` 确认，`getrusage03.h` 实际扫描格式为：

```text
VmSwap: %lu
/proc/self/status
VmSwap is not zero
```

因此 `/proc/self/status` 需要至少提供 `VmSwap` 字段，且值为 0。

### 根因分析

| 问题 | 根因 |
|------|------|
| `/proc/self/status` ENOENT | proc 生成只覆盖 `/proc/<pid>/stat` 和 `/proc/<pid>/maps`，`openat` 也只重定向 `stat/maps` |
| `initial.children = 0` | `waitpid` 回收子进程时没有把子进程资源累计到父进程 `cutime/cstime/ru_maxrss` |
| 孙进程资源不向上传递 | 子进程 wait 孙进程后的累计资源没有保存到进程退出快照 |
| `/proc/<pid>/stat` ENOENT | 子进程退出时立即删除 `/proc/<pid>`，但 zombie 在父进程 wait 前仍可能被读取 |
| 300MiB 场景 SIGSEGV | QEMU 与内核物理内存只有 256MiB，且 `mmap` 单次限制为 64MiB，无法真实触碰 300MiB 测试页 |
| `Expected 1 conversions got 0` | `/proc/self/status` 缺少 LTP 扫描的 `VmSwap: %lu` 字段 |

### 修复

#### 1. `/proc/<pid>/status`

`os/src/fs/kernel_fs_ops/proc_file.rs`：

- 创建 `/proc/<pid>/status`
- 支持刷新当前 RSS / 虚拟内存大小
- 输出 `VmSwap: 0 kB`，满足 `getrusage03.h` 的 swap 检查
- 保留 `VmHWM` / `VmRSS` / `VmSize` 等常见字段

`os/src/syscall/fs/fd_ops.rs`：

- `open("/proc/self/status")` 重定向到 `/proc/<pid>/status`
- 打开前刷新当前进程 status

#### 2. `ru_maxrss`

`os/src/mm/memory_set/mod.rs`：

- 新增 `resident_size_kb()`：按已分配物理页计算 RSS
- 新增 `virtual_size_kb()`：按 VMA 范围计算 VmSize

`os/src/timer/rusage.rs` / `os/src/syscall/time.rs`：

- `Rusage` 新增带 `maxrss` 的构造函数
- `RUSAGE_SELF` 返回当前 RSS
- `RUSAGE_CHILDREN` 返回 wait 累计的 `cmaxrss`

#### 3. wait 资源累计

`os/src/task/process/process.rs`：

- 新增 `ProcessUsage`，在进程退出时冻结资源快照

`os/src/task/mod.rs`：

- 最后一个线程退出时记录自身 `utime/stime/maxrss`
- 同时记录该进程已 wait 子进程的 `cutime/cstime/cmaxrss`

`os/src/syscall/task/wait.rs`：

- wait 成功回收时，父进程累计：
  - `cutime += child.utime + child.cutime`
  - `cstime += child.stime + child.cstime`
  - `cmaxrss = max(child.maxrss, child.cmaxrss)`
- 修正过滤后 child 下标不能直接删除原始 children 列表的问题，改为按 pid 定位

#### 4. proc 生命周期

原来进程最后一个线程退出时立即删除 `/proc/<pid>`，导致 zombie 阶段读取 `/proc/<pid>/stat` 失败。

修复为：

- exit 时保留 `/proc/<pid>`
- `Process::remove_from_global_map()` 即父进程 wait 回收时删除 proc 文件

#### 5. 300MiB 触页场景

`getrusage03` 会真实触碰 100MiB / 300MiB 内存。当前 256MiB QEMU 内存不够，因此同步调整：

- `make_scripts/riscv64.mk` / `make_scripts/loongarch64.mk`：`MEMORY_SIZE = 512M`
- 两架构 `memory_layout.rs`：`PHYSICAL_MEMORY_SIZE = 512MiB`
- `USER_HEAP_SIZE` / `MAX_BRK_SIZE` / `MAX_MMAP_SIZE` 调整到 512MiB
- `sys_mmap` 去掉 64MiB 单次限制，改为按总 mmap 上限判断

### 涉及文件

| 文件 | 修改内容 |
|------|----------|
| `os/src/fs/kernel_fs_ops/proc_file.rs` | 新增 status 生成/刷新，补 `VmSwap` |
| `os/src/syscall/fs/fd_ops.rs` | `/proc/self/status` 重定向并刷新 |
| `os/src/mm/memory_set/mod.rs` | RSS / VmSize 统计 |
| `os/src/timer/rusage.rs` | `ru_maxrss` 构造 |
| `os/src/timer/timedata.rs` | 新增 `cmaxrss` |
| `os/src/syscall/time.rs` | `getrusage` 填充 `ru_maxrss` |
| `os/src/task/process/process.rs` | 新增进程资源快照，wait 回收时删除 proc |
| `os/src/task/mod.rs` | 退出时冻结资源快照 |
| `os/src/syscall/task/wait.rs` | wait 后累计子进程资源 |
| `make_scripts/*.mk` / `memory_layout.rs` | 512MiB 运行与内核内存布局 |

### 验证

RISC-V：

```text
make all
timeout 150s make run
```

`getrusage03` 输出：

```text
getrusage03.c:43: TPASS: initial.self ~= child.self
getrusage03.c:57: TPASS: initial.children ~= 100MB
getrusage03.c:66: TPASS: child.children == 0
getrusage03.c:84: TPASS: child.children ~= 300MB
```

`timeout` 最终结束 QEMU，因此命令退出码为 124；在超时前测例四项检查均已 TPASS，未再出现 `/proc/self/status` TBROK 或 StorePageFault。
