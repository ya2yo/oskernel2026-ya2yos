# preliminary LoongArch iozone-glibc 共享库搜索路径

## 背景

初赛 LoongArch64 测试镜像将 glibc 放在 `/glibc/lib`，而 iozone 的 glibc
版本是动态链接 ELF。内核启动时已为 legacy preliminary 镜像补齐 ELF
`PT_INTERP` 所需的 `/lib64/ld-linux-loongarch-lp64d.so.1` 别名。

## 现象

`iozone-glibc` 的 automatic measurements 阶段在动态解释器启动后退出：

```text
./iozone: error while loading shared libraries: libc.so.6: cannot open shared object file: No such file or directory
```

## 分析

内核目前按 Linux ELF/VFS 边界精确打开 `PT_INTERP`，不会在内核中重写动态
链接器的依赖库路径。该策略是正确的；`DT_NEEDED` 的搜索应由 glibc loader 完成。

LoongArch 的 legacy glibc loader 在没有 `ld.so.cache` 或
`LD_LIBRARY_PATH` 时会搜索 `/usr/lib64`。原兼容层只创建了
`/lib64/libc.so.6 -> /glibc/lib/libc.so.6`，因此解释器本身可以执行，但在
`/usr/lib64/libc.so.6` 查找失败。通过设置全局 `LD_LIBRARY_PATH` 绕过会让
musl/glibc 子进程继承混合库搜索路径，破坏镜像隔离和正常 `exec` 语义。

## 根因

legacy preliminary 镜像的启动期文件兼容层遗漏了 LoongArch glibc loader 实际
使用的 `/usr/lib64` 库目录，仅覆盖了 ELF 解释器路径和 `/lib64`。

## 修复

`os/src/fs/kernel_fs_ops/initfiles.rs` 的
`create_legacy_test_loader_alias()` 在 `target_arch = "loongarch64"` 分支中：

- 确保 `/usr` 和 `/usr/lib64` 存在；
- 仅在路径缺失时，创建 `libc.so`、`libc.so.6`、`libm.so`、`libm.so.6` 到
  `/glibc/lib` 的符号链接；
- 继续只对 legacy preliminary 镜像执行，最终镜像和镜像已有的目录/文件不会被
  覆盖。

这让 glibc loader 保持自己的正常搜索行为，同时把测试镜像的非标准库布局以
标准 VFS 路径呈现出来。

## 涉及文件

- `os/src/fs/kernel_fs_ops/initfiles.rs`

## 验证

已执行：

```bash
make
git diff --check
```

根 `Makefile` 的该次构建完成 RISC-V64 与 LoongArch64 release 构建，均通过；
只有既有 Cargo config 弃用提示和 vendored `smoltcp` warning。`git diff --check`
通过。

尝试两次 LoongArch64 preliminary QEMU：首次 180 秒运行已完成 iozone-musl 的
前七项，但尚未抵达 `iozone-glibc`；第二次 300 秒运行未推进至目标阶段，且
`timeout` 未自动回收 QEMU 子进程，已手动终止该专属进程。因此本次没有将
`iozone-glibc` 标记为 QEMU 运行通过，仍需在可定向运行该组的测试入口上复测。
