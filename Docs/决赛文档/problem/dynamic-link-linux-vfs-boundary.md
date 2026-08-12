# 动态链接路径去硬编码与 Linux ELF/VFS 边界

## 背景

`os/src/fs/map_dynamic_link.rs` 原本为竞赛镜像维护 `/glibc/lib`、`/musl/lib` 的共享库白名单，并按 basename 把用户态动态加载器的请求改写到固定目录；读取特定 libc 时还按固定文件偏移替换机器码。这些规则依赖某一份镜像和某一版工具链。

## 现象

当根文件系统包含 Debian multiarch、Alpine 或其它版本的 libc 时，内核可能把 loader 声明的路径或 RPATH 探测路径替换成另一套 libc。动态加载器随后看到不匹配的私有 ABI、符号版本或 TLS 布局，表现为 `ENOENT` 被伪造成成功、重定位失败、SIGSEGV 或取指故障。

## 分析

Linux 的内核职责是执行 ELF 的 `PT_INTERP`：打开 ELF 中的精确路径，映射解释器的 `PT_LOAD` 段并将控制权交给解释器。解释器负责 `DT_NEEDED`、`RPATH/RUNPATH`、`ld.so.cache`、符号链接、版本选择和重定位。动态库候选路径不存在时，VFS 必须保留 `ENOENT`，不能通过 basename 回退提前改变用户态加载器的搜索顺序。

## 根因

兼容层把镜像布局知识放进了通用 `open/openat` 和 ext4 读取路径：路径表、固定前缀、当前工具链版本、架构特定库名和 ELF 文件偏移共同构成了不可移植的隐式 ABI。

## 修复

- 删除动态库路径表、basename fallback、架构/工具链路径白名单和 libc 机器码补丁。
- `open()`、`openat()` 和 ext4 inode 读取按请求路径及底层文件原样工作。
- `PT_INTERP` 只通过 `open_direct()` 打开 ELF 声明的精确路径；不存在时让 `execve` 失败，不再回退到 `/glibc` 或 `/musl`。
- 保留空的 `map_dynamic_link` 模块作为边界说明，防止未来重新引入镜像专用策略。

## 涉及文件

- `os/src/fs/map_dynamic_link.rs`
- `os/src/fs/kernel_fs_ops/open.rs`
- `os/src/syscall/fs/fd_ops.rs`
- `os/src/mm/memory_set/elf_loader.rs`
- `os/src/fs/ext4_lw/inode/io.rs`
- `os/src/fs/ext4_lw/inode/mod.rs`
- `os/src/fs/mod.rs`
- `os/src/fs/kernel_fs_ops/mod.rs`

## 验证

已完成本次涉及 Rust 文件的定向 `rustfmt`、`git diff --check`，并确认源码中没有旧动态映射/补丁 API 的调用残留。`make TARGET_ARCH=riscv64` 与 `make TARGET_ARCH=loongarch64` 均成功；尚未运行完整 QEMU、LTP 或 BuildStorm 动态链接回归。
