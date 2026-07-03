# tst_virt: /proc/cpuinfo 缺失导致 TBROK

## 背景

LTP 公共库 `lib/tst_virt.c` 会在部分用例启动阶段调用 `tst_is_virt()` 探测虚拟化环境。该路径会先尝试 `systemd-detect-virt`，若不可用或未识别，再读取 `/proc/cpuinfo`，扫描是否包含 `QEMU Virtual CPU` 来判断 KVM。

Ya2yOS 当前没有完整 procfs，而是在启动期通过 `create_init_files()` 写入少量测试依赖的 `/proc` 兼容文件，例如 `/proc/mounts`、`/proc/meminfo` 和 `/proc/sys/kernel/*`。因此新增这类基础 proc 文件应优先放在启动伪文件初始化中，而不是在 `openat()` 热路径里特殊判断。

## 现象

修复前 `log.ans` 中失败集中在 `/proc/cpuinfo` 打开阶段：

```text
[sys_openat] path is /proc/cpuinfo, flags is O_LARGEFILE, mode is 666
open(/proc/cpuinfo,O_LARGEFILE,438)
abs_path is /proc/cpuinfo
tst_virt.c:37: TBROK: fopen(/proc/cpuinfo,r) failed: ENOENT (2)
```

这不是业务 syscall 本身的断言失败，而是 LTP harness 在虚拟化探测阶段无法读取基础 proc 文件，提前以 `TBROK` 中断。

## 分析

`/proc/meminfo` 在同一份日志中可以正常打开，说明 `/proc` 目录和普通启动伪文件写入流程已经可用。继续检查 `os/src/fs/kernel_fs_ops/initfiles.rs` 后确认，启动期只创建了：

- `/proc/mounts`
- `/proc/meminfo`
- `/proc/sys/kernel/tainted`
- `/proc/sys/kernel/pid_max`
- `/proc/sys/kernel/core_pattern`

没有 `/proc/cpuinfo`。因此 `sys_openat()` 解析到绝对路径 `/proc/cpuinfo` 后，最终走普通 ext4 查找并返回 `ENOENT`。

LTP `tst_virt.c` 对 `/proc/cpuinfo` 的需求很低，只要求 `fopen()` 成功并能逐行读取。它只在内容包含 `QEMU Virtual CPU` 时返回 KVM。若我们为了绕过 `ENOENT` 写入该字符串，可能改变测试对虚拟化环境的判断，影响依赖 `VIRT_KVM` 或 `VIRT_ANY` 的分支。因此本次提供 Linux 风格的最小 CPU 信息，但不包含 `QEMU Virtual CPU`，让探测函数正常返回“未识别为 KVM”，避免额外行为变化。

## 根因

启动期 proc 兼容文件缺少 `/proc/cpuinfo`，导致 LTP 公共虚拟化探测库在测试主体运行前读取该文件失败并报 `TBROK`。

## 修复

在 `os/src/fs/kernel_fs_ops/initfiles.rs` 中新增架构条件的 `CPUINFO` 常量：

- RISC-V 提供 `processor`、`hart`、`isa`、`mmu`、`uarch` 等最小字段。
- LoongArch64 提供 `processor`、`cpu family`、`model name`、`CPU Revision`、`FPU` 等最小字段。

随后在 `create_proc_files()` 中和 `/proc/mounts`、`/proc/meminfo` 一样调用 `write_init_file("/proc/cpuinfo", CPUINFO)`，保证系统启动后该路径可被普通 `openat()` 和 `fopen()` 读取。

修复刻意不写入 `QEMU Virtual CPU`，避免 LTP `tst_virt.c` 把当前环境识别为 KVM。

## 涉及文件

| 文件 | 修改 |
|------|------|
| `os/src/fs/kernel_fs_ops/initfiles.rs` | 新增 RISC-V/LoongArch64 最小 `/proc/cpuinfo` 内容，并在启动期创建 `/proc/cpuinfo` |

## 验证

已执行：

```text
make
```

结果：默认 RISC-V 构建通过，仅有既有 warning。

AI 曾尝试在沙箱内运行：

```text
timeout 120s make run > /tmp/cpuinfo-fix.log 2>&1
```

QEMU 因沙箱内 `/var/tmp` 只读无法创建临时文件而未启动：

```text
qemu-system-riscv64: ... Could not open temporary file '/var/tmp/...': Read-only file system
```

随后维护者在可运行环境中完成验证，并确认最新输出已写入 `log.ans`。AI 读取最新 `log.ans`，未再出现原先的 `tst_virt.c:37: TBROK` 或 `/proc/cpuinfo` `ENOENT`，summary 为：

```text
Summary:
passed   7
failed   0
broken   0
```

未执行 `TARGET_ARCH=loongarch64` 构建或运行验证；本次修复包含 LoongArch64 的静态 `CPUINFO` 内容，但实际验证基于默认 RISC-V 构建和维护者提供的最新 `log.ans`。
