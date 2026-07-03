# RISC-V Alpine 镜像启动期 initfiles 与动态链接路径兼容

## 背景

为了在当前内核中尝试运行 Alpine 镜像里的 `/usr/bin/vim`，RISC-V 运行目标切换到 `2026_testsuits_img/onsite-2026/alpine-linux-riscv64-ext4fs.img`。该镜像是普通 Alpine/musl 根文件系统，目录布局为 `/bin`、`/lib`、`/usr` 等，不包含竞赛测试镜像里的 `/musl` 与 `/glibc` 顶层目录。

## 现象

内核启动阶段在 `create_init_files()` 中 panic：

```text
[kernel] Panicked at src/fs/kernel_fs_ops/initfiles.rs:61 called `Result::unwrap()` on an `Err` value: ENOENT
```

修掉该 panic 后，又出现 `/bin/sh` 执行失败：

```text
exec /bin/sh failed: -2
```

继续修正后，启动期不再 panic，但 Alpine `/bin/sh` 动态程序进入用户态后又触发：

```text
Exception(FetchInstructionPageFault) in application, bad addr = 0xfffffffffffffffe
```

该取指 fault 来自动态解释器回退路径仍使用会再次映射动态库路径的通用 `open()`，导致 Alpine 原始解释器没有真正按原路径打开。

## 分析

`initfiles.rs::flush_libgcc_s()` 无条件将内嵌的 `libgcc_s.so.1` 写入 `/glibc/lib/libgcc_s.so.1`。这只适用于竞赛 glibc 测试镜像；Alpine 镜像没有 `/glibc/lib`，因此 `open(... O_CREATE ...)` 返回 `ENOENT`，随后被 `unwrap()` 放大为 kernel panic。

此外，`create_bin_files()` 无条件创建 `/bin/* -> /musl/busybox` 链接和 LTP wrapper。Alpine 镜像本来已有 `/bin/sh -> /bin/busybox`，但启动期会将其覆盖成指向不存在的 `/musl/busybox`，导致 `execve("/bin/sh")` 返回 `ENOENT`。

最后，ELF loader 对动态解释器路径调用 `map_dynamic_link_file_directly_map()`，会把 `/lib/ld-musl-riscv64.so.1` 映射到竞赛镜像路径 `/musl/lib/libc.so`。Alpine 中真实解释器在 `/lib/ld-musl-riscv64.so.1`，因此映射路径打开失败时需要回退到 ELF `.interp` 原始路径。原回退实现仍调用通用 `open()`，而通用 `open()` 内部会再次执行动态库路径映射，所以 fallback 实际仍落回 `/musl/lib/libc.so`，解释器加载失败又被 `Option::None` 当作“无解释器”，最终让动态 ELF 以错误入口返回用户态。

## 根因

启动期兼容逻辑把固定竞赛测试镜像布局当作所有根文件系统的通用布局：

- 假设 `/glibc/lib` 一定存在，用于写入 `libgcc_s.so.1`。
- 假设 `/musl/busybox` 一定存在，用于生成 `/bin` applet 链接。
- 假设 musl 动态解释器一定要映射到 `/musl/lib/libc.so`。
- ELF loader 用 `Option` 同时表示“静态 ELF 无解释器”和“动态 ELF 解释器加载失败”，导致失败被静默吞掉。

这些假设在 Alpine RISC-V 镜像下均不成立。

## 修复

- `flush_libgcc_s()` 改为只在 `/glibc/lib` 存在时写入 `libgcc_s.so.1`，并移除可失败路径上的 `unwrap()`。
- `create_bin_files()` 增加 `/musl/busybox` 探测；只有竞赛测试镜像存在该文件时才创建 `/bin` applet 链接和 LTP wrapper。Alpine 镜像保留原生 `/bin/sh`。
- 新增 `open_direct()`，允许内核按精确路径打开文件而不经过动态库兼容映射。
- `elf_loader` 打开动态解释器时先尝试兼容映射路径；如果映射路径不存在且不同于 ELF 原始 `.interp`，则用 `open_direct()` 回退打开原始路径。
- `load_dl_interp_if_needed()` 改为返回 `Result<Option<usize>, ()>`，区分“静态 ELF 无解释器”和“动态 ELF 解释器加载失败”，避免加载失败后继续以错误入口返回用户态。

涉及文件：

- `os/src/fs/kernel_fs_ops/initfiles.rs`
- `os/src/fs/kernel_fs_ops/open.rs`
- `os/src/fs/kernel_fs_ops/mod.rs`
- `os/src/fs/mod.rs`
- `os/src/mm/memory_set/elf_loader.rs`

## 验证

已执行：

```text
make TARGET_ARCH=riscv64
timeout 25s make run TARGET_ARCH=riscv64 > /tmp/ya2yos-riscv-start.log 2>&1
```

结果：

- RISC-V 构建通过。
- QEMU 日志显示 `fs::init...create_init_files success!`，原 `initfiles.rs:61 libgcc_s ENOENT` panic 消失。
- `/bin/sh` 不再以 `exec /bin/sh failed: -2` 返回，说明 `/bin/sh` 没有再被覆盖到不存在的 `/musl/busybox`。
- 修复 `open_direct()` 与解释器失败错误传播后，25 秒 QEMU 日志中不再出现 `Panicked`、`Exception`、`FetchInstructionPageFault`、`SIGSEGV` 或 `exec /bin/sh failed`；QEMU 由 `timeout` 终止，说明系统已越过原动态解释器取指 fault。
