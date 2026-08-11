# LoongArch clang 取指异常：动态库混用导致错误回调

## 背景

LoongArch BuildStorm 并发编译期间，clang 进程曾在同一个高地址反复报告
`FetchInstructionPageFault`：

```text
sepc=stval=0xfffffffffffe4c44
signal=SIGSEGV
```

该地址最初被误判为 signal/vDSO 的固定 `rt_sigreturn` 入口，随后又被误判为可以兼容返回
`-ENOSYS` 的普通 helper。两种做法都只是掩盖了已经损坏的用户控制流。

## 分析

从实际 LoongArch 镜像提取并反汇编 loader 后，故障现场的返回地址
`ra=0x1500001084` 位于 `_dl_catch_exception` 的普通间接调用之后：

```asm
0x1078: ld.d  $r12, $r3, 8
0x1080: jirl  $r1, $r12, 0
0x1084: ...
```

`_rtld_global_ro` 中 `+0x138` 是 `_dl_catch_error`，而不是当前 loader 期望的
`_dl_find_object` 槽。错误跳转后 callback 为 0，最终从
`0xfffffffffffe4c44` 取指；因此该地址是错误间接调用的结果，不是 ABI 入口。

`LD_DEBUG=libs` 给出了决定性证据。修复前，原生 clang 的 RUNPATH 探测：

```text
trying file=/usr/bin/../lib/libc.so.6
calling init: /usr/bin/../lib/libc.so.6
```

被内核 basename 兼容层静默改为 `/glibc/lib/libc.so.6`。但该程序使用的解释器是
Debian GLIBC 2.41：

```text
/lib64/ld-linux-loongarch-lp64d.so.1
Debian GLIBC 2.41-12
```

而 `/glibc/lib/libc.so.6` 是旧的 GLIBC 2.38 库。两个版本的 loader 与 libc 私有
`_rtld_global_ro` 布局不兼容，loader 因此把错误槽当成 `_dl_find_object`，callback=0
并跳到上述高地址。故障不是 TLB、Hart 激活、`ibar`、DMW、eager ELF 映射或 QEMU
`jirl` 解码问题。

## 根因

`os/src/fs/map_dynamic_link.rs` 原先对任意共享库请求按 basename 回退到旧的
`/glibc/lib` 或 `/musl/lib`。对原生 `/usr`、`/bin`、Rust toolchain RUNPATH 的探测，
正确语义应是返回 `ENOENT`，让 Debian loader 继续搜索
`/lib/loongarch64-linux-gnu`；兼容层却把旧库伪装成成功打开，造成 native loader 与旧
libc 混用。

## 修复

新增 `current_executable_uses_legacy_library_store()`，以当前进程的 executable provenance
决定是否允许 basename 回退：

- `/glibc` 或 `/musl` 下启动的历史程序仍可使用兼容重定向；
- 原生 `/usr`、`/bin`、Rust toolchain 等程序保留原始路径和 `ENOENT`，继续由 loader
  搜索 multiarch 目录；
- 程序明确请求 `/glibc/lib/...` 或 `/musl/lib/...` 时仍保留该显式路径；
- native multiarch、GCC 和 binutils plugin 路径继续绕过兼容回退。

删除了所有把 `0xfffffffffffe4c44` 映射成可执行 helper、返回 `-ENOSYS` 或据此修改
Hart 指令同步的临时实验。Ya2yOS 私有 signal restorer 仍使用独立的
`sigreturn_va()`，不依赖该错误地址。

## 涉及文件

- `os/src/fs/map_dynamic_link.rs`
- `Docs/决赛文档/problem/loongarch-signal-vdso-sigreturn.md`

## 验证

修复后 `LD_DEBUG=libs` 已显示 native multiarch 库：

```text
trying file=/lib/loongarch64-linux-gnu/libc.so.6
calling init: /lib/loongarch64-linux-gnu/libc.so.6
```

clang 串行 50 次和三轮并发压力（每轮 600 次）全部通过，共 1850 次执行；没有
`WARN`、`FetchInstructionPageFault`、`SIGSEGV` 或 panic，并输出：

```text
CLANG_STRESS_PASS
CLANG_STRESS_STATUS 0
```

正式 LoongArch `make run` 的完整 BuildStorm 结果以根目录 `log.ans` 中最后的
`BUILDSTORM_COMPILE`、测试组结束标记和 `shutdown!` 为准。
