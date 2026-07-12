# LTP mmap04 `/proc/self/maps` 动态映射与格式修复

## 背景

LTP `mmap04` 在每个子用例中先建立两页匿名映射，再以 `MAP_FIXED`
重新映射第二页，并从 `/proc/self/maps` 查找该页的起始地址和权限字符串。
测试覆盖读、写、执行权限以及 `MAP_PRIVATE`、`MAP_SHARED` 的全部 14 种组合。

## 现象

`log.ans` 中 musl 和 glibc 版本均在首个子用例报：

```text
mmap04.c:64: TBROK: Expected 1 conversions got 0 FILE '/proc/self/maps'
```

两者 summary 都是 `passed 0 failed 0 broken 1`。

## 分析

原实现只在进程创建时生成一次 `/proc/<pid>/maps`，之后 `mmap`、`munmap`
和 `MAP_FIXED` 拆分 VMA 都不会更新文件。因此 proc 文件无法反映当前地址空间。

调试运行确认读取 `/proc/self/maps` 时刷新路径已经写入了内容，但仍不能被
`mmap04` 的 `scanf("%" PRIxPTR "-%%*x %%s", ...)` 匹配。原因是实现固定使用
`{:016x}` 输出起始地址，产生前导零；Linux maps 使用无前导零的十六进制地址。

此外，`MAP_FIXED` 覆盖已有 VMA 时复用了 `mprotect()` 拆分逻辑，但该逻辑只更新
权限和后备文件，未更新 `mmap_flags`。即使 maps 内容刷新，第二页仍会沿用第一段的
私有/共享标志，导致共享映射显示为 `p`。

## 根因

1. `/proc/<pid>/maps` 被实现为创建期快照，没有在 open 时基于当前 VMA 刷新。
2. maps 地址使用固定 16 位格式，不符合 Linux procfs 文本格式和 LTP 的匹配方式。
3. `MAP_FIXED` 的 VMA 拆分没有把新映射的 `MAP_SHARED/MAP_PRIVATE` 属性写回目标区间。

## 修复

- 新增 `refresh_proc_maps()`：在 `MemorySet` 读锁内复制 VMA 元数据，释放地址空间锁
  后再重建 `/proc/<pid>/maps`，避免持锁进入 VFS；刷新以 `O_TRUNC` 打开文件，防止
  映射减少后残留旧行。
- `sys_openat()` 在 `/proc/self/maps` 解析为真实 pid 后，以及直接打开
  `/proc/<pid>/maps` 时调用刷新函数。
- maps 行按 `{:x}-{:x}` 输出地址；权限第四列根据 `MAP_SHARED` 输出 `s`，否则输出 `p`。
- 为 `MemorySetInner::mprotect()` 增加可选的 mmap flags 参数。`MAP_FIXED` 拆分/覆盖
  VMA 时传入新 flags，普通 `mprotect(2)` 不传入，从而保留其原有共享属性。

## 涉及文件

- `os/src/fs/kernel_fs_ops/proc_file.rs`
- `os/src/fs/kernel_fs_ops/mod.rs`
- `os/src/fs/mod.rs`
- `os/src/syscall/fs/fd_ops.rs`
- `os/src/mm/memory_set/mmap_ops.rs`
- `os/src/mm/memory_set/handle.rs`

## 验证

执行：

```text
make
make log
timeout 120s make run > /tmp/proc-self-maps-final.log 2>&1
make TARGET_ARCH=riscv64
```

默认 LoongArch64 的 `mmap04` 单跑中，musl 和 glibc 各 14 个权限组合均为 `TPASS`，
summary 均为 `passed 14 failed 0 broken 0 skipped 0 warnings 0`。默认 LoongArch64
和 RISC-V 构建均通过。构建仅有既有 `smoltcp` vendor warning。
