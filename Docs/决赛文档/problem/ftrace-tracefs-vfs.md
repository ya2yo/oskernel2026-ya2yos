# onsite ftrace tracefs 虚拟文件系统

## 背景

`2026_testsuits_img/onsite-2026/rv.img` 中的 `/glibc/ftrace_testcode.sh` 测试
`/sys/kernel/tracing` 的 tracing_on、trace、trace_mode 和 max_entries 四个
tracefs 节点，覆盖读写控制、list/tree 输出、事件捕获、环形缓冲区上限和关闭
tracing 后不产生新事件。

## 现象

修复前镜像没有 `/sys/kernel/tracing` 目录及节点，测试四项均无法通过。首次接入
动态节点后，T1、T2、T3 通过，但 T4 将每个内部事件渲染出的 ENTER/EXIT 两行都
计入上限，`max_entries=4` 时得到 8 行，超过脚本要求。

## 分析

现有 VFS 允许在 `open_inner()` 中为 `/proc/uptime` 返回不落盘的 `File` 实现，
普通用户读写统一经 `File::read`/`File::write`。因此 tracefs 适合采用同样的动态
文件对象：启动阶段只创建目录和占位文件，运行时按精确路径返回 tracing 状态视图。
普通 ext4 文件打开成功时记录一个 `vfs_open` 事件，足以覆盖脚本对 `ls`、`cat`
等命令的触发需求。

## 根因

内核缺少 tracefs 的路径、状态和事件缓冲区；并且 list 模式一个逻辑调用对应
ENTER/EXIT 两个可见事件行，内部容量若直接等于 `max_entries` 会违反测例按行计数
的上限。

## 修复

- 新增 `os/src/fs/files/tracing.rs`，实现四个动态文件的读写、seek、poll、stat、
  list/tree 输出、tracing 开关、模式切换、清空缓冲区和事件上限。
- `os/src/fs/kernel_fs_ops/initfiles.rs` 启动创建 `/sys/kernel/tracing` 目录树和
  四个占位文件；有任务运行后由动态 tracefs 对象接管，不依赖测试脚本修改。
- `os/src/fs/kernel_fs_ops/open.rs` 将四个精确路径接入公共 VFS 打开路径，并在普通
  inode 打开成功时记录 `vfs_open` 事件。
- list 模式按可见 ENTER/EXIT 行折半计算内部事件容量；tree 模式按事件条目计算。
  这样 `max_entries=4` 时 list 输出最多四条可计数行，同时仍保留至少一个事件。

## 涉及文件

- `os/src/fs/files/tracing.rs`：tracefs 动态文件和全局 tracing 状态。
- `os/src/fs/files/mod.rs`：导出 `TracingFile`。
- `os/src/fs/kernel_fs_ops/mod.rs`：接入打开模块依赖。
- `os/src/fs/kernel_fs_ops/open.rs`：动态路径分派和 VFS 事件记录。
- `os/src/fs/kernel_fs_ops/initfiles.rs`：启动目录、占位节点和默认值。

## 验证

- `debugfs -R 'cat /glibc/ftrace_testcode.sh' 2026_testsuits_img/onsite-2026/rv.img`：
  确认脚本路径、四个节点、list/tree 字段和 T4 行数判据。
- `make TARGET_ARCH=riscv64 build-arch`：通过。
- `make TARGET_ARCH=loongarch64 build-arch`：通过。
- `timeout 180s make TARGET_ARCH=riscv64 run`（沙箱外 QEMU snapshot）：最终输出
  `FTRACE TEST1 PASSED`、`FTRACE TEST2 PASSED`、`FTRACE TEST3 PASSED`、
  `FTRACE TEST4 PASSED`；无 panic，随后 BuildStorm 正常启动。
- `git diff --check`：通过。

