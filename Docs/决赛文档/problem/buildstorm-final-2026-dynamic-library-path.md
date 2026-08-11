# BuildStorm final-2026 动态库路径错误重定向

## 背景

`final-2026` RISC-V 根文件系统是 Debian glibc 镜像。它同时保留旧竞赛兼容目录
`/glibc/lib`，以及 Debian 原生 multiarch 库目录
`/lib/riscv64-linux-gnu`、`/usr/lib/riscv64-linux-gnu`。内核原有的动态库兼容层会在
`openat` 前根据共享库 basename 将未知路径改写到旧兼容目录。

## 现象

初始 `log.ans` 中，`timeout`、`head`、`nproc` 先后报告：

```text
/lib/riscv64-linux-gnu/libc.so.6: version `GLIBC_2.38' not found
```

修复这条明显的版本错误后，`rustc --version` 仍收到 `SIGSEGV`，BuildStorm 的
`BUILDSTORM_TOOLCHAIN` 为 `fail`。debug 日志进一步显示 Rust toolchain 的
RPATH 探测请求 `/root/.rustup/.../lib/libc.so.6` 被改写成
`/glibc/lib/libc.so.6`；故障 PC 位于该旧 libc 的 `getrandom` TLS 访问，非
`librustc_driver` 本体。

## 分析

镜像只读核对结果如下：

| 路径 | libc 版本范围 | 作用 |
| --- | --- | --- |
| `/lib/riscv64-linux-gnu/libc.so.6` | `GLIBC_2.27` 至 `GLIBC_2.41` | final Debian 原生 libc |
| `/glibc/lib/libc.so.6` | 至多 `GLIBC_2.35` | 旧竞赛兼容副本 |

原实现有两处破坏动态链接器搜索语义。

1. ELF loader 先按兼容规则改写 `PT_INTERP`，即使镜像中声明的解释器路径真实存在。
2. `map_dynamic_link_file()` 对任意不在白名单内的绝对 `.so` 路径按 basename 回退到
   `/glibc/lib` 或 `/musl/lib`。Rustup toolchain 的 RPATH 目录本身没有
   `libc.so.6`；正确行为应为该次 `open` 返回 `ENOENT`，让 native `ld-linux`
   继续搜索 `/lib/riscv64-linux-gnu`，而不是伪造旧 libc 打开成功。

因此，原生 loader 与旧 libc 被混用；旧 libc 的 TLS 私有 ABI 不再与 final Debian
loader 匹配，最终在 `getrandom` 的 TLS 访问处触发 SIGSEGV。

## 根因

动态链接兼容层把历史镜像的 basename 回退规则错误扩大到 final Debian 的原生 multiarch
目录和应用 RPATH/RUNPATH 目录，改变了本应由动态链接器处理的 `ENOENT` 与后续搜索顺序。

## 修复

- `os/src/mm/memory_set/elf_loader.rs`：打开 `PT_INTERP` 时先以 ELF 原始 `.interp`
  路径执行 `open_direct()`；仅原路径不存在时才回退历史 `/glibc`/`/musl` 映射。
- `os/src/fs/map_dynamic_link.rs`：保护 RISC-V、LoongArch64 的 Debian native multiarch
  路径，不再重定向到旧兼容库。
- 将 basename 回退限制为历史链接器根目录 `/lib/`、`/usr/lib/`、`/lib64/`、
  `/usr/lib64/`、`/glibc/`、`/musl/`。其它绝对路径，包括 Rustup toolchain 的
  RPATH，按原样打开并保留 `ENOENT` 语义。

## 涉及文件

- `os/src/fs/map_dynamic_link.rs`
- `os/src/mm/memory_set/elf_loader.rs`

## 验证

执行：

```text
make
timeout 120s make run TARGET_ARCH=riscv64 > log.ans 2>&1
```

结果：

- RISC-V 与 LoongArch64 release 构建均通过。
- `GLIBC_2.38 not found` 不再出现。
- `rustc 1.98.0-nightly` 与 `cargo 1.98.0-nightly` 均正常输出，
  `BUILDSTORM_TOOLCHAIN ok` 已写入根目录 `log.ans`。
- 当前宿主只能运行 QEMU `2G / 2 CPU`，而 final 测例正式配置要求 `8G / 8 CPU`。
  在该受限配置下，后续静默 minibuild 编译持续满载运行并被外层 timeout 终止，未将
  完整 BuildStorm 编译或性能分项记为通过。

## 2026-08-11：LoongArch native loader 与旧 libc 混用

LoongArch clang 的新故障表面为 `_dl_find_object` 路径中的
`FetchInstructionPageFault`，`sepc=stval=0xfffffffffffe4c44`。实际解释器是 Debian
GLIBC 2.41，而内核把 native clang 的 `/usr/bin/../lib/*.so` 探测按 basename 改写到
`/glibc/lib` 的旧 GLIBC 2.38 库。两套版本的 `_rtld_global_ro` 私有布局不兼容，loader
把 `_dl_catch_error` 槽当成 `_dl_find_object`，callback=0 后才跳到该高地址；这不是
TLB、Hart 激活、`ibar` 或 signal restorer 问题。

`map_dynamic_link.rs` 现在只允许 executable provenance 位于 `/glibc` 或 `/musl` 的历史
程序使用 basename 兼容回退。原生 `/usr`、`/bin` 和 Rust toolchain 的失败探测保留
`ENOENT`，由 Debian loader 继续搜索 `/lib/loongarch64-linux-gnu` 等 multiarch 目录；
显式 `/glibc/lib/...` 请求仍可工作。LoongArch native `libc`、`libm` 和 `libgcc_s` 已由
`LD_DEBUG=libs` 确认从 multiarch 目录加载。

串行 50 次与三轮并发 600 次 clang 压力全部通过，共 1850 次；正式 BuildStorm 的最终
`BUILDSTORM_COMPILE` 与 shutdown 结果记录在根目录 `log.ans`。
