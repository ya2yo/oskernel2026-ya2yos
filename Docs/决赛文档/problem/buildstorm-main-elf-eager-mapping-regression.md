# BuildStorm 主 ELF 全量 eager 映射回归

## 背景

提交 `ed27f13bed92da1490b7218ff39e382ce9438dde` 将动态 ELF 的所有
`PT_LOAD` 段恢复为 eager framed 映射，用以修复 `libctest` 动态程序在动态
链接器初始化期间退出的问题。该修复对 LTP 动态用例是必要的。

此前主程序 `execve()` 的页对齐 `PT_LOAD` 段采用 `MAP_PRIVATE` file-backed
lazy VMA，只有非页对齐段仍由 loader eager 读取。该路径专门用于避免 BuildStorm
反复执行大型 Rust 工具时预先复制未访问的代码和只读数据。

## 现象

`log.ans` 中 CAgent 已完成，BuildStorm 的非计时 `tg-xtask` 预构建也完成。进入
`arceos-helloworld` 的正式 Cargo 构建后，Cargo 启动 RISC-V `rustc` 失败：

```text
error: could not compile `core` (lib)
could not execute process `.../bin/rustc ...` (never executed)
Bad file descriptor (os error 9)
```

错误发生前 `compiler_builtins`、`core`、`libc` 和 `std` 已开始编译；因此
`QEMU: Terminated` 是构建失败后的结束现象，不是根因。

## 分析

`ed27f13...` 直接改写了 `map_elf_lazy_file()`。该函数不仅由
`load_dl_interp_if_needed()` 加载动态解释器，也由 `from_elf_file()` 加载每次
`execve()` 的主程序。改动移除了页对齐段的 `MAP_PRIVATE` lazy 分支，令主 ELF
和解释器都在 `execve()` 内逐段分配物理帧并从文件复制全部内容。

这保留了动态解释器的安全启动条件，却撤销了主 ELF 的按需加载边界。BuildStorm 会高频
启动体积较大的 `rustc` 及其辅助进程，因而在该回归后于 Cargo 的子进程启动阶段暴露
`EBADF`。日志中的 Cargo errno 是用户态可见症状；本次修复针对其已知的内核映射回归，
不修改 fd 表或通过改变测试脚本规避问题。

`libctest-dynamic-elf-eager-load.md` 的运行结果表明需要 eager 的是动态解释器启动路径：
解释器在 libc 尚未可用时会修改自身重定位状态，不能依赖当前 file-backed fault/COW 路径。
主程序不需要共享该限制。

## 根因

主 ELF 和动态解释器复用了同一个 loader 函数，导致为修复解释器启动而引入的 eager
策略被扩大到所有 `execve()` 主程序。大型 BuildStorm 子进程失去 file-backed lazy
`PT_LOAD` 映射，破坏既有的资源使用和执行边界。

## 修复

在 `os/src/mm/memory_set/elf_loader.rs` 拆分两条路径：

- `map_elf_eager_file()` 仅用于 `load_dl_interp_if_needed()` 加载动态解释器；所有
  `PT_LOAD` 段建立 framed 映射并从文件填充，保持 LTP 动态 ELF 所需的重定位语义。
- `map_elf_lazy_file()` 仅用于 `from_elf_file()` 加载主程序；页对齐的文件段恢复
  `MAP_PRIVATE` file-backed lazy VMA，`.bss` 保持匿名 lazy VMA。
- 主程序非页对齐 `PT_LOAD` 段仍使用 `push_elf_segment_from_file()` eager 填充，保留
  段首/段尾零填充、共享页和文件末页语义。

普通用户 `mmap`、fd 表、调度和测试脚本均未修改。

## 涉及文件

- `os/src/mm/memory_set/elf_loader.rs`
- `Docs/决赛文档/problem/buildstorm-main-elf-eager-mapping-regression.md`

## 验证

- `rg -a -n -C 20 'Bad file descriptor|could not execute process|error: could not compile' log.ans`：
  定位到上述 BuildStorm 首个失败上下文。
- `make TARGET_ARCH=riscv64`：通过。该根目录构建流程同时完成 RISC-V 与
  LoongArch64 release 构建；仅有 vendored `smoltcp` 的既有 unused warning。
- `git diff --check`：通过。

本轮按维护者要求未运行新的 QEMU、LTP 或完整 BuildStorm。后续应以当前
`initproc` 定向入口重新执行 RISC-V BuildStorm，确认 `rustc` 不再出现 `EBADF`；同时
回归动态 LTP/libctest，确认动态解释器继续走 eager 路径。
