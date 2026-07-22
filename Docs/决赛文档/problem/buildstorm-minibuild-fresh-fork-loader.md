# BuildStorm MINIBUILD fresh fork TrapContext 与 libc/loader 混配

## 背景

final-2026 的 BuildStorm 在工具链检查后会先创建 `/tmp/minibuild`，再在干净项目中运行
`cargo build`。单独执行 `buildstorm_minibuild_build_debug.sh` 可以复用已有 target，顺利输出
`BUILDSTORM_DEBUG_MINIBUILD ok`；按 `prepare -> build` 顺序运行则必须触发 Cargo 的 fresh
fork、Rustc 和 GCC 链接路径。

本问题承接 [BuildStorm MINIBUILD mmap 预算与动态栈 fork EFAULT](./buildstorm-minibuild-post-toolchain-stall.md)
中的前序修复。前序修复消除了 `MAP_STACK` 漏继承导致的 `CLONE_CHILD_SETTID` `EFAULT`，但
fresh 路径仍先卡死，随后在解除卡死后暴露出 linker 错误。

## 现象

原始 `log.ans` 在如下序列后被外部 10 分钟 timeout 终止，既没有 `ok`，也没有脚本主动
输出的 `fail`：

```text
BUILDSTORM_DEBUG_MINIBUILD_PREPARE begin
BUILDSTORM_DEBUG_MINIBUILD_PREPARE ok
BUILDSTORM_DEBUG_MINIBUILD_BUILD begin
```

临时仅对 debug 脚本保留 `cargo build` 的 stderr 后，修复卡死得到两个按顺序暴露的 linker
错误：

```text
undefined reference to symbol '__tls_get_addr@@GLIBC_2.27'
/lib/riscv64-linux-gnu/ld-linux-riscv64-lp64d.so.1: error adding symbols: DSO missing from command line
```

以及：

```text
/lib/riscv64-linux-gnu/libc.so.6:
undefined reference to `__tunable_is_initialized@GLIBC_PRIVATE'
```

## 分析

### 普通 fork 覆盖了正确的 TrapContext

普通 fork 已经把当前调用线程的 `parent_trap_cx` 结构体快照写入 child 独立的
TrapContext 页。随后旧代码又调用：

```rust
child_mm.clone_area(
    VirtAddr::from(child_inner.trap_cx_bottom).floor(),
    &parent_ref,
);
```

在多线程 Cargo 中，child 新分配的 trap VMA 虚拟地址在 parent 地址空间中可能对应另一个
线程的 TrapContext 页，而不是发起 `clone()` 的线程。该整页复制会覆盖刚刚写入 child 的
正确快照。

RISC-V probe 证明 child 本应从 glibc `_Fork` 的 clone 返回点继续执行，却在复制后带有另一
Cargo 线程的 `futex_wait` `sepc`、`ra`、`sp` 和 `tp`。虽然 `a0` 之后被重设为 0，child
仍从错误的 futex wait 现场恢复。该 futex 带 `FUTEX_PRIVATE_FLAG`，child 进程不会接到 parent
的 wake，因此形成永久等待，表现为 10 分钟外层 timeout。

### 原生 libc 被 legacy 动态加载器替换

镜像的 `/usr/lib/riscv64-linux-gnu/libc.so` 是 GNU ld script：

```ld
GROUP (
  /lib/riscv64-linux-gnu/libc.so.6
  /usr/lib/riscv64-linux-gnu/libc_nonshared.a
  AS_NEEDED ( /lib/ld-linux-riscv64-lp64d.so.1 )
)
```

原生 `libc.so.6` 将 `__tls_get_addr@GLIBC_2.27` 和
`__tunable_is_initialized@GLIBC_PRIVATE` 留给同版本的 native
`/usr/lib/riscv64-linux-gnu/ld-linux-riscv64-lp64d.so.1`。后者定义两个符号，并通过
`/lib -> usr/lib` 的镜像符号链接可访问。

旧兼容层有两次重定向：`sys_openat` 先调用 `map_dynamic_link_file()`，随后 `open()` 再调用
`map_library_path()`。它们把 linker script 中的 raw
`/lib/ld-linux-riscv64-lp64d.so.1` 改写为 `/glibc/lib/ld-linux-riscv64-lp64d.so.1`。
该 legacy loader 不定义 `__tunable_is_initialized@GLIBC_PRIVATE`，而且只对应较旧的 glibc
版本，故不能与 final 镜像的 native libc 混用。

此外，`/usr/lib/gcc/<arch>/...` 曾被按 `/usr/lib/` 前缀误判为 legacy 搜索路径。GCC 的
`libgcc_s.so` linker script 随后搜索 `libgcc_s.so.1` 时会被错误改写到 `/glibc/lib`，破坏
原生 toolchain 的库选择。

## 根因

1. task-private TrapContext 被误当成可按同一虚拟地址从 parent 克隆的普通 VMA，覆盖了实际
   发起 fork 的线程现场。
2. 动态库兼容层无条件用 legacy loader 和 legacy `libgcc_s` 替换 final Debian 镜像的原生
   工具链路径，导致 native libc 与不兼容 loader 混配。

## 修复

- `os/src/task/task/task.rs`：删除 child TrapContext VMA 的 `clone_area()`。child 已由
  `parent_trap_cx` 快照初始化独立页，不应再从 parent 地址空间按 child 地址复制。
- `os/src/fs/map_dynamic_link.rs`：将 RISC-V/LoongArch GCC toolchain 目录纳入 native 路径，
  并为 `ld-*` 动态加载器路径保留原始 pathname，避免在第一层 mapper 被 basename fallback
  改写。
- `os/src/fs/kernel_fs_ops/open.rs`：在第二层 mapper 前用 `open_direct()` 检查 loader 原路径。
  镜像存在真实 loader 时保持 native libc/loader 配对；原路径不存在的旧镜像仍 fallback 到
  既有 `/glibc/lib` 或 `/musl/lib` 兼容目标。

`os/src/fs/mod.rs` 只导出上述共享的 loader-path 判定 helper。诊断期间临时打开的 Cargo stderr
已恢复为原来的 `>/dev/null 2>&1`，不会改变正式 debug 脚本的 fd/pipe 拓扑。

后续确认 `clone_area()` 在移除错误调用点后已无任何业务调用，因此同时删除了
`os/src/mm/memory_set/area_ops.rs` 中的 eager-copy 实现和
`os/src/mm/memory_set/handle.rs` 中的包装接口，避免未来再次把按虚拟地址复制的 helper 用于
task-private VMA。

## 验证

- `rustfmt --check os/src/fs/map_dynamic_link.rs os/src/fs/mod.rs os/src/fs/kernel_fs_ops/open.rs os/src/task/task/task.rs`：通过。
- `git diff --check`：通过。
- `make build-arch TARGET_ARCH=riscv64`：通过。
- `make build-arch TARGET_ARCH=loongarch64`：通过；仅有既有 Cargo config 弃用和 vendored
  smoltcp warnings。
- `timeout 360s make run TARGET_ARCH=riscv64 > /tmp/buildstorm-minibuild-loaderfix-riscv.log 2>&1`：
  guest 主动结束，输出：

  ```text
  BUILDSTORM_DEBUG_MINIBUILD_PREPARE ok
  BUILDSTORM_DEBUG_MINIBUILD ok
  shutdown!
  ```

  日志没有 `panic`、`TFAIL`、`TBROK`、`ERROR` 或 `WARN`；启动期 `sigaltstack` 与 rseq
  regression 也均为 `PASS`。

本轮未运行完整官方 `buildstorm_testcode.sh` 的后续 `cargo xtask` 编译阶段，结论仅覆盖已经
实际触发并通过的 MINIBUILD fresh 路径。
