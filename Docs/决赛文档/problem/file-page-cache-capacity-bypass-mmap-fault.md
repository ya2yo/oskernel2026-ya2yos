# BuildStorm 文件页缓存满后 mmap 缺页误杀动态程序

## 背景

RISC-V final-2026 的 `riscv.ans` 从 2026-08-01 13:38:38 写到 14:58:18，宿主文件
生命周期约 79 分 40 秒。日志末尾的 guest 时间为 `t=4739263ms`，与该持续时间一致。
该运行不是正常关机：没有 `shutdown!`，末尾为 `QEMU: Terminated`。

`buildstorm_testcode.sh` 的 `END buildstorm` 只是脚本在 `cargo` 命令后无条件执行的
`echo`，不代表 `initproc` 已完成回收并调用 `shutdown()`。脚本之后的 `sync` 也已经段错误。
因此，`shutdown!` 缺失说明 guest 未走到 `user/src/lib.rs` 的正常关机路径；仅从日志不能
确定宿主具体的终止命令，但约 80 分钟的外层生命周期和 `QEMU: Terminated` 证明 QEMU 在正常
关机前被外部终止。

## 现象

文件页缓存容量为 `96 * 1024 = 98304` 页，即 4 KiB 页下 384 MiB。原日志首次触顶时为：

```text
[perf] file_cache_capacity resident_pages=98304 max_pages=98304 capacity_bypass_pages=707
```

最终统计为 `capacity_bypass_pages=2580`。在触顶后，原先仍在运行的 Bash 继续打印脚本尾部，
但新启动的动态 glibc 程序相继失败：

```text
buildstorm_testcode.sh: line 55: 88 Segmentation fault cargo build -p tg-xtask
buildstorm_testcode.sh: line 61: 947 Segmentation fault timeout 14400 cargo xtask ...
buildstorm_testcode.sh: line 61: 946 Segmentation fault tee /work/buildstorm.build.out
buildstorm_testcode.sh: line 76: 958 Segmentation fault tail -25 /work/buildstorm.build.out
buildstorm_testcode.sh: line 79: 959 Segmentation fault sync
```

这解释了为什么 `END buildstorm` 可见而 `shutdown!` 不可见：它不是成功结束标记，后续程序
已经无法加载，QEMU 又在 `initproc` 正常关机前被终止。

## 分析

`FilePageCache::get_or_load()` 在缓存已满时仍会从底层文件读取并成功返回 `Arc<FilePage>`，
但不会把该页发布到全局 `FILE_PAGE_CACHE`。这是有意保持 384 MiB 上限的 capacity-bypass 行为。

文件 mmap 缺页为避免在 `MemorySet` 写锁内进入 EXT4，原先先在锁外调用
`prepare_file_page()`，但其返回值只是 `Option<bool>`，实际取得的 `Arc<FilePage>` 被丢弃。锁内
的 `mmap_read_page_fault()` / `mmap_write_page_fault()` 只能再次通过 `FILE_PAGE_CACHE.get_inode()`
查询。

缓存未满时第二次查询命中，问题被掩盖；缓存满时 bypass 页从未发布，第二次查询返回 `None`。
处理器把这个“已有可读取文件页但未在全局缓存”的情形当作映射失败，trap 层最终向用户进程发送
`SIGSEGV`。动态链接器、`libc.so` 或刚执行文件的首个文件映射页都可能触发该错误。

`fork()` 中 `MAP_SHARED` 文件页预取也存在相同边界：预取页在容量满时可读取却不在全局缓存，
之后锁内安装阶段若只查缓存同样会丢页。

## 修复

- `MemorySet::prepare_file_page()` 现在返回 `Option<Arc<FilePage>>`，并将该页传入锁内缺页处理。
- `page_fault_handler` 先验证该准备页仍匹配当前 VMA 的 inode cache path 和 page index；VMA 被并发
  `munmap`/替换时不会错误安装旧页。验证失败才回退查询全局缓存。
- `mmap_read_page_fault()`、`mmap_write_page_fault()` 和 mmap 分派链携带这个可选页。`MAP_PRIVATE`
  仍使用 COW 语义，`MAP_SHARED` 仍直接映射共享文件帧。
- `prefetch_shared_file_pages()` 保留 `MAP_SHARED` fork 预取得到的页，并按 `FilePageKey` 传给安装点。

修复不扩大生产缓存上限，也不在持有 `MemorySet` 写锁时读取 EXT4。容量旁路页只保留到当前缺页
安装完成，随后由该 VMA 持有其 frame。

## 独立复现

新增 `buildstorm::cache_capacity` 诊断 case。仅在显式启用内核 feature
`file-cache-capacity-test` 时，缓存上限从生产的 96K 页降为 2,048 页（8 MiB）；默认构建不改变
容量。case 顺序读取镜像内两个各小于 32 MiB 普通读取准入阈值的 Rustup 二进制，使缓存实际填满，
然后执行此前未访问过的 `/usr/bin/tail`。

临时将 `test_final_2026()` 的正式 BuildStorm 调用替换为：

```rust
buildstorm::cache_capacity::run();
```

再执行：

```bash
make build-arch TARGET_ARCH=riscv64 KERNEL_EXTRA_FEATURES=perf,file-cache-capacity-test
timeout 120s make run TARGET_ARCH=riscv64 > /tmp/file-page-cache-capacity-riscv.log 2>&1
rg -a -n 'BUILDSTORM_DEBUG_CACHE_CAPACITY|file_cache_capacity|Segmentation fault|shutdown!' \
  /tmp/file-page-cache-capacity-riscv.log
```

验证后必须恢复正式的：

```rust
run_final_testsuit("glibc\0", "buildstorm_testcode.sh\0");
```

该 feature 和 case 只用于定向回归，不能用于最终评分镜像。

## 验证

为确认定向 case 真正覆盖旧错误，验证时只在 `file-cache-capacity-test` 构建中临时丢弃锁外已加载
页，等价于修复前的二次缓存查询行为；临时代码未保留。

| 版本 | 关键结果 |
| --- | --- |
| 修复前等价路径 | `resident_pages=2048 max_pages=2048 capacity_bypass_pages=1365`，`read_page_miss=2924`、`read_bypass_file_ops=0`，case 子进程 `status=139`。 |
| 修复后 | `resident_pages=2048 max_pages=2048 capacity_bypass_pages=1382`，同样 `read_page_miss=2924`、`read_bypass_file_ops=0`，输出 `BUILDSTORM_DEBUG_CACHE_CAPACITY ok` 与 `shutdown!`，无 `Segmentation fault`。 |

已执行：

- `cargo fmt --manifest-path os/Cargo.toml --all`
- `cargo fmt --manifest-path user/Cargo.toml --all`
- `git diff --check`
- `make build-arch TARGET_ARCH=riscv64`
- `make build-arch TARGET_ARCH=loongarch64`
- 使用 `perf,file-cache-capacity-test` 的 RISC-V 修复前等价路径和修复后 QEMU 定向运行

两个 release 构建均通过。未运行 LoongArch64 QEMU，因为该独立 case 依赖 RISC-V final-2026
镜像内的 Rustup 路径；共享 MM 代码已由 LoongArch64 编译覆盖。

## 涉及文件

| 文件 | 修改 |
| --- | --- |
| `os/src/mm/memory_set/handle.rs` | 保留锁外加载的文件页，并为 fork 返回预取页。 |
| `os/src/mm/memory_set/mmap_ops.rs` | 将准备页传过 mmap 缺页分派。 |
| `os/src/mm/page_fault_handler.rs` | 校验并优先安装准备页，回退缓存查找。 |
| `os/src/mm/memory_set/fork_clone.rs` | `MAP_SHARED` fork 安装时使用预取页。 |
| `os/Cargo.toml`、`os/src/fs/page_cache.rs` | 增加默认关闭的定向测试容量 feature。 |
| `user/src/bin/buildstorm/cache_capacity.rs` | 独立的 capacity-bypass QEMU 回归 case。 |
