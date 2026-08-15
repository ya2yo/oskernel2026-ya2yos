# Preliminary basic 脚本权限与动态解释器路径

## 背景

`initproc` 已按已挂载镜像内容选择 preliminary 或 final 测例。根 Makefile 的 RISC-V 和
LoongArch64 QEMU 默认镜像均为 `2026_testsuits_img/pre_tests`，因此一次普通 `make run` 会进入
`test_pre()`，不会运行 `test_final_2026()`。

## 现象

原始 `log.ans` 在内核启动后输出：

```text
detected preliminary test image; running preliminary suites
basic_testcode.sh: line 3: ./run-all.sh: Permission denied
#### OS COMP TEST GROUP END basic-musl ####
```

musl 与 glibc 两组随后均结束并关机，basic 子程序并未执行。修复脚本权限后，所有子程序又统一
打印 `exec: OOM during ELF load`。

## 分析

使用 `debugfs` 检查两种预赛镜像，`/musl/basic/run-all.sh` 和
`/glibc/basic/run-all.sh` 的 mode 都是 `0644`，但 `basic_testcode.sh` 使用
`./run-all.sh` 直接执行。Linux 兼容的 `execve` 必须拒绝这个请求，不能为了测试脚本放宽执行位检查。

将脚本 mode 修正后，读取 `basic/brk` 的 program header 可见：

```text
Requesting program interpreter: /lib/ld-linux-riscv64-lp64d.so.1
```

镜像根目录只有 `/glibc` 和 `/musl`，真实 loader 位于
`/glibc/lib/ld-linux-riscv64-lp64d.so.1`。`MemorySetInner::load_dl_interp_if_needed()` 依法按
PT_INTERP 的原始绝对路径打开文件，失败后 `TaskControlBlock::exec()` 的泛化错误日志将该失败显示成
“OOM during ELF load”，并不表示 CMA 或物理页耗尽。

LoongArch64 preliminary 镜像同样将 loader 存在 `/glibc/lib/`，其 ELF 请求的是
`/lib64/ld-linux-loongarch-lp64d.so.1`。

## 根因

固定 preliminary 测试镜像的文件布局不满足其自身脚本与 ELF 声明：一是 `run-all.sh` 漏了执行位，
二是根级 `/lib` 或 `/lib64` loader 别名没有物化。此前日志在第一处失败，掩盖了第二处问题。

## 修复

- `user/src/bin/initproc.rs` 新增 preliminary basic 运行器，先以 BusyBox 把仅该 wrapper 的
  `basic/run-all.sh` 改为 `0755`，再执行原始 `basic_testcode.sh`；任一组返回非零即停止并返回失败。
- `os/src/fs/kernel_fs_ops/initfiles.rs` 在存在 legacy `/musl/busybox` 且根 loader 目录缺失时建立目录和
  精确 loader 符号链接：RISC-V `/lib/ld-linux-riscv64-lp64d.so.1`，LoongArch64
  `/lib64/ld-linux-loongarch-lp64d.so.1`，均指向镜像已有的 `/glibc/lib` 文件。
- final 根文件系统已有 `/lib` 或 `/lib64` 时不创建任何别名；ELF loader 仍按原始 `PT_INTERP`
  路径处理，不恢复宽泛 basename 动态库重定向。

## 涉及文件

- `user/src/bin/initproc.rs`
- `os/src/fs/kernel_fs_ops/initfiles.rs`
- `Docs/决赛文档/problem/preliminary-basic-script-interpreter.md`

## 验证

- `make run TARGET_ARCH=riscv64`：默认 preliminary 镜像的 musl/glibc basic 各运行 32 项，全部输出
  对应结束标记，末尾为 `shutdown!`；未出现 `Permission denied`、`OOM during ELF load`、panic、TFAIL
  或 TBROK。
- `make build-arch TARGET_ARCH=riscv64`：通过。
- `make build-arch TARGET_ARCH=loongarch64`：通过。
- 未运行 LoongArch64 QEMU preliminary 全路径，故该架构的 loader 别名目前仅通过构建与镜像 ELF/目录
  检查验证。

## 后续：netperf 动态库路径

### 现象

同一 preliminary 镜像切换到 `netperf_testcode.sh` 后，musl 组的 `netperf` 与 `netserver` 都显示
`exec: OOM during ELF load` / `Out of memory`；glibc 组可以进入动态加载器，但所有项目均显示：

```text
error while loading shared libraries: libm.so.6: cannot open shared object file
```

### 分析与根因

镜像内 `/musl/netperf` 的 `PT_INTERP` 是 `/lib/ld-musl-riscv64-sf.so.1`，并需要 `libc.so`；
`/glibc/netperf` 的 `PT_INTERP` 是 `/lib/ld-linux-riscv64-lp64d.so.1`，其 `DT_NEEDED` 包含
`libm.so.6` 与 `libc.so.6`。但实际镜像将上述文件分别放在 `/musl/lib/` 和 `/glibc/lib/`，根目录
没有完整的 `/lib` 标准路径。

此前只补齐了 glibc loader，因此 musl 在打开 `PT_INTERP` 时仍失败，而 glibc loader 启动后找不到
`libm.so.6`。`TaskControlBlock::exec()` 当前仍把这类 ELF 装载错误统一映射为 `ENOMEM`，故日志中的
OOM 不是物理内存不足。

### 后续修复

`create_legacy_test_loader_alias()` 现仅对同时满足 legacy musl 标记、且不含 final 镜像标记的根文件
系统生效。它即使 `/lib` 或 `/lib64` 已存在，也逐项确保缺失的别名存在：

- RISC-V：补齐 glibc/musl loader、`libc.so`、`libc.so.6`、`libm.so`、`libm.so.6` 到实际
  `/glibc/lib/` 或 `/musl/lib/` 文件的符号链接。
- LoongArch64：以 `/lib64` 的对应 ABI 路径补齐同类 loader 与库别名。

此修复不改变 `MemorySetInner::load_dl_interp_if_needed()` 的精确 `PT_INTERP` 打开语义，也不恢复
按 basename 搜索动态库的兼容逻辑；它只让固定旧镜像提供其 ELF 已声明的绝对文件名。

### 后续验证

- `make run TARGET_ARCH=riscv64`：musl 与 glibc 的 `UDP_STREAM`、`TCP_STREAM`、`UDP_RR`、
  `TCP_RR`、`TCP_CRR` 各五项均输出 `end: success`，两组均到达结束标记与 `shutdown!`；未出现
  `OOM during ELF load`、`Out of memory` 或共享库缺失错误。
- `make build-arch TARGET_ARCH=loongarch64`：通过。
- 未运行 LoongArch64 QEMU 的 netperf 全路径，因此该架构的运行期别名行为仍待设备/QEMU 环境复测。
