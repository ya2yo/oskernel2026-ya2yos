# 龙芯架构 iozone-glibc 动态链接与缺页处理修复

### 初始情况

龙芯架构运行 glibc 版 iozone 报错：

```bash
Error relocating ./iozone: __isoc23_sscanf: symbol not found
Error relocating ./iozone: __isoc23_strtoll: symbol not found
Error relocating ./iozone: __isoc23_strtol: symbol not found
```

riscv 版报错：

```bash
panic at src/mm/translate.rs:118 called `Option::unwrap()` on a `None` value
```

### 修复过程

#### 1. 修复 LoongArch glibc ld-linux 路径映射错误

**根因**：`os/src/fs/map_dynamic_link.rs:55` 将 glibc 的动态链接器错误映射到了 musl 的 libc.so。

```rust
// 原代码（错误）
"/lib64/ld-linux-loongarch-lp64d.so.1" => Some("/musl/lib/libc.so"),
```

iozone 是 glibc 编译的动态链接 ELF，其 `.interp` 段指定解释器为 `/lib64/ld-linux-loongarch-lp64d.so.1`。内核把这个路径映射到了 musl 的 libc.so 作为解释器加载。musl 不提供 glibc 2.38 特有的 `__isoc23_*` C23 符号，导致重定位失败。

riscv 能正常运行动态链接是因为对应的映射是正确的：

```rust
"/lib/ld-linux-riscv64-lp64d.so.1" => Some("/glibc/lib/ld-linux-riscv64-lp64d.so.1"),
```

**修改**（`os/src/fs/map_dynamic_link.rs`）：

- 第 55 行：`"/lib64/ld-linux-loongarch-lp64d.so.1"` 映射目标改为 `"/glibc/lib/ld-linux-loongarch-lp64d.so.1"`
- `DYNAMIC_PATH` 集合中添加 `"/glibc/lib/ld-linux-loongarch-lp64d.so.1"`

#### 2. 修复 libc.so.6 路径映射缺失

修复 ld-linux 映射后，glibc 的动态链接器成功加载，但尝试加载 `libc.so.6` 时报错：

```bash
./iozone: error while loading shared libraries: libc.so.6: cannot open shared object file: No such file or directory
```

通过打开 debug 日志确认，LoongArch 的 glibc 动态链接器的默认搜索路径为 `/usr/lib64/`（而非 `/lib64/`），调用序列为：

```bash
openat(/etc/ld.so.cache)     → ENOENT（无缓存，跳过）
openat(/usr/lib64/libc.so.6) → 需要映射到 /glibc/lib/libc.so.6
```

**修改**（`os/src/fs/map_dynamic_link.rs`）：在 `map_library_path` 和 `DYNAMIC_PATH` 中补充以下映射：

```rust
"/usr/lib64/libc.so.6" => Some("/glibc/lib/libc.so.6"),
"/lib64/libc.so.6"     => Some("/glibc/lib/libc.so.6"),
"/lib/libc.so.6"       => Some("/glibc/lib/libc.so.6"),
"/glibc/libc.so.6"     => Some("/glibc/lib/libc.so.6"),
```

至此，libc.so.6 被正确找到、打开并 mmap 映射。

#### 3. 修复 execve 后 clear_child_tid 指向旧地址空间导致 panic

libc.so.6 加载成功后，iozone 顺利运行到实际调用系统调用阶段，但 `statx` 系统调用发生 panic：

```bash
[kernel] Panicked at src/mm/translate.rs:118 called `Option::unwrap()` on a `None` value
```

**根因**：`sys_statx`（`os/src/syscall/fs/stat.rs:138`）使用了不安全的 `translated_str` 函数读取用户空间路径指针。该函数内部直接调用 `translate_va().unwrap()`，不处理缺页。用户空间路径字符串所在的页面还未被延迟分配映射，导致 `.unwrap()` 直接 panic。

这与此前 riscv 的 `sys_fstatat` 问题同根——都是系统调用中用 `translated_str` 替代 `copy_from_user` 导致的。

**修改**（`os/src/syscall/fs/stat.rs`）：将 `sys_statx` 第 138 行的 `translated_str` 替换为 `copy_from_user` 模式：

```rust
// 原代码
let path = translated_str(memory_set.token(), path);

// 修改后
let mut dst_str = [0u8; MAX_PATH_LEN];
copy_from_user(&memory_set, path as usize, &mut dst_str);
let len = dst_str.iter().position(|&b| b == 0).unwrap_or(MAX_PATH_LEN);
let path_str = core::str::from_utf8(&dst_str[..len]).unwrap_or("");
let path_str = trim_start_slash(String::from(path_str));
```

#### 4. 预防性修复：execve 时提前处理 clear_child_tid

在分析 panic 过程中发现一个隐患：`execve` 系统调用通过 `change_memory_set_and_sigtable` 将当前活跃地址空间替换为新 ELF 的地址空间。如果 `clear_child_tid` 不为零，该地址指向旧地址空间，替换后在新地址空间中无对应映射，进程退出时会 panic。

**修改**（`os/src/task/task/task.rs`）：在 `exec()` 中，`change_memory_set_and_sigtable` 之前，在旧地址空间中完成 `clear_child_tid` 的写零和 futex_wake 操作，然后将其清零。

```rust
if task_inner.clear_child_tid != 0 {
    let old_memory_set = self.process.get_locked_memory_set_read();
    let _ = copy_to_user(&old_memory_set, task_inner.clear_child_tid as usize, &[0u8; 4]);
    if let Some(pa) = old_memory_set.translate_va(VirtAddr::from(task_inner.clear_child_tid)) {
        futex_wake_up(pa.0, 1);
    }
    drop(old_memory_set);
    task_inner.clear_child_tid = 0;
}
```

### 涉及文件

| 文件 | 改动内容 |
| ------ | --------- |
| `os/src/fs/map_dynamic_link.rs` | 修复 LoongArch ld-linux 映射，添加 libc.so.6 路径映射 |
| `os/src/syscall/fs/stat.rs` | `sys_statx` 中 `translated_str` → `copy_from_user` |
| `os/src/task/task/task.rs` | `exec()` 中提前处理 `clear_child_tid` |
