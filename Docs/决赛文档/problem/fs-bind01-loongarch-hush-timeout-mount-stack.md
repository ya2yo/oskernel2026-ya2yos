# LoongArch LTP fs_bind01 hush timeout 与 bind 挂载栈卸载修复

## 背景

维护者在 LoongArch64、`pre_tests/sdcard-la.img` 上单跑 musl LTP
`fs_bind01.sh`，根目录 `log.ans` 没有到达挂载传播断言，而是在 LTP 公共 shell 库初始化
watchdog 时提前结束。修正该前置问题后，测例进入真实的 bind 挂载栈回收阶段，又暴露两处
连续 `umount` 的 `EINVAL`。

该镜像的 BusyBox 配置将 `/bin/sh` 设为 hush（
`testsuits-for-oskernel/config/busybox-config-loongarch64` 中
`CONFIG_SH_IS_HUSH=y`）。RISC-V 配置使用 ash，因此不能把两种 shell 的局部变量行为混为一谈。

## 现象

原始 `log.ans` 在 `tst_test.sh::_tst_multiply_timeout()` 中显示：

```text
+ local timeout=300
...
+ '[' '' -ge 1 ']'
sh: out of range
timeout need to be >= 1 ()
timeout per run is 0h 0m 0s
```

LTP 随后用零秒参数启动 `tst_timeout_kill`，立即终止 shell。后续的 `/proc/<pid>/stat`
读取或 `kill` 清理报错都是 watchdog 终止后的次生现象，不能据此判断 `fs_bind01` 的挂载语义。

修正 watchdog 后，测例已能完成 propagation 检查，但曾出现：

```text
umount: can't unmount parent1/child1: Invalid argument
umount: can't unmount share2: Invalid argument
```

两个路径的第一次 `umount` 成功，第二次却失败。LTP 脚本明确要求对它们各执行两次成功卸载：
先弹出较新的 bind 层，再弹出较早的 self-bind 层。

## 分析

### hush 的 `eval local` 兼容性

LTP 原脚本用动态变量名读取超时参数：

```sh
eval "local timeout=\$$1"
```

在镜像内 BusyBox 1.33 hush 中，该命令的 trace 会显示执行了
`local timeout=300`，但同一函数随后读取的 `$timeout` 仍为空。比较操作因此报
`out of range`，而后续算术把调用者的 `sec` 赋为 `0`。

在真实镜像 BusyBox 加 LoongArch 用户态模拟器的独立复现中，将声明与赋值拆开：

```sh
local timeout
eval "timeout=\$$1"
```

可以得到 `timeout=300`，同时保持函数局部变量边界，不污染调用者。

### `/proc/mounts` 让 BusyBox 一次卸掉两层

watchdog 修复后，内核 `MountTable` 追踪显示两组 mount event 都正确保留了不同层：

```text
parent1/child1: self-bind event 8, disk1 bind event 10
share2:         self-bind event 6, parent2 bind event 11
```

`MountTable::umount()` 按精确路径选择顶层，再只删除该层所属 event group；这部分没有
错误。问题发生在内核把表导出给 BusyBox 的边界：旧的 `/proc/mounts` 序列化将 bind
mount 的 `special` 原样放在第一列。对 self-bind 而言，第一列 source 与第二列 mountpoint
相同，例如：

```text
.../parent1/child1 .../parent1/child1 ...
```

LoongArch BusyBox 未启用 mtab support，`umount` 直接倒序读取 `/proc/mounts`。它既以第二列
匹配 mountpoint，也以第一列匹配 device；当用户执行 `umount parent1/child1` 时，先匹配到上层
mountpoint，随后又把下层 self-bind 的第一列视为同一 device。于是一个用户态 `umount` 发起两次
`umount2`，错误地同时移除了上层和下层，下一条 LTP `umount` 才收到 `EINVAL`。`share2`
的失败路径相同。

## 修复

### 启动期 LTP 库兼容补丁

`os/src/fs/kernel_fs_ops/initfiles.rs` 在创建 BusyBox wrapper 前检查并补丁两份 LTP 公共库：

- `/musl/ltp/testcases/bin/tst_test.sh`
- `/glibc/ltp/testcases/bin/tst_test.sh`

补丁仅替换精确的旧语句为分开的 `local timeout` 和 `eval "timeout=..."`，找不到旧语句时不写入，
因此重复启动时幂等。已存在脚本直接写回原 inode 并在成功写入后截断，保留其路径、类型和权限；
镜像本身保持不变，修补发生在 Ya2yOS 启动后的文件系统视图中。缺失某一 libc 树时忽略 `ENOENT`，
其他 I/O 错误继续返回给初始化路径。

### bind source 的 `/proc/mounts` 表示

`os/src/fs/mount.rs::MountTable::proc_mounts_content()` 现在仅在输出 `/proc/mounts` 时，将
`MS_BIND` 条目的 source 渲染为非路径的 `none`。真正的 `MountEntry.special` 不变，仍供
bind propagation、子目录偏移和 VFS 镜像逻辑使用；`is_bind` 作为独立属性保留，因此后续
`MS_REMOUNT` 覆盖可见 flags 后不会把 bind mount 重新显示为路径 source。非 bind 挂载继续
输出真实 source，保留按设备名卸载的既有行为。

Linux 通常报告 bind mount 的 backing filesystem source。当前路径化 VFS 尚未保存一个可精确
导出的 backing device 标识，故使用稳定的合成 source `none`，避免 BusyBox 将 mountpoint
路径误当作 device。该改动不放宽内核 `umount` 的错误处理，也不把不存在的挂载伪装为成功。

## 涉及文件

- `os/src/fs/kernel_fs_ops/initfiles.rs`
- `os/src/fs/mount.rs`
- `Docs/决赛文档/problem/fs-bind01-loongarch-hush-timeout-mount-stack.md`

## 验证

在 LoongArch64、8 GiB/8 CPU、pre-tests 镜像配置下执行：

```bash
make TARGET_ARCH=loongarch64
timeout 60s make run TARGET_ARCH=loongarch64 > /tmp/fs-bind01-normal-final.log 2>&1
make TARGET_ARCH=riscv64
```

该运行正常到达 `shutdown!`。LTP 脚本自身 Summary 为：

```text
passed   29
failed   0
broken   0
skipped  0
warnings 0
```

两次 `umount parent1/child1` 和两次 `umount share2` 均为 `TPASS`；日志中不再出现
`timeout need to be >= 1`、`timeout per run is 0h 0m 0s`、`TFAIL`、`TBROK` 或 panic。

调试入口曾使用 `sh -x`，其外层 `LtpOutputScanner` 会将 trace 中的字符串（包括 trap 文本的
`TBROK`）重复计数，因此外层的 `passed 203 / broken 2` 不是用例结论。本次采用 LTP 脚本自身
的 Summary 作为验收结果。RISC-V release 构建通过，未额外运行 RISC-V QEMU；本次 shell 兼容
问题由 LoongArch hush 触发，RISC-V 仍使用 ash。
