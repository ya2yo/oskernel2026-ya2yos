# LTP mmap13 文件映射 EOF 外页 SIGBUS 修复

## 背景

LTP `mmap13` 创建一个长度为半页的普通文件，将其以两个页的长度进行
`MAP_SHARED | PROT_READ | PROT_WRITE` 映射，然后向第二个完整页面写入。Linux 要求
该访问投递可捕获的 `SIGBUS`；映射尾页中仍在 EOF 所在页内的字节则保持零填充。

## 现象

最初 musl 与 glibc 都输出：

```text
mmap13.c:62: TFAIL: SIGBUS signal not received
Summary:
passed   0
failed   1
broken   0
```

补充 EOF 检查后，进程又在 LTP 安装 `SIGBUS` handler 之前以退出码 `135` 终止。运行日志
显示误触发的 VMA 是 LTP 框架创建后 unlink 的 `/dev/shm/ltp_mmap13_<pid>` 共享页，而不是
测试自身的 `mmapfile`。

## 分析

原 mmap read/write fault handler 对文件映射没有区分 EOF，`FilePageCache` 在页起点超过
文件长度时会建立 `valid_len = 0` 的零页。因此第二个完整 EOF 外页不会产生 `SIGBUS`。

不能在 fault 时单纯调用路径式 `inode.size()`：LTP 框架会在 mmap 后 unlink 其共享文件，
当前 ext4 适配层无法再通过原路径可靠查询该对象长度。VMA 需要保留建图时的文件长度。

排查时还发现 `Ext4Inode::truncate()` 调用的 lwext4 `file_truncate(4096)` 虽然返回成功，
随后的底层 path-based `stat` 仍可能报告 `st_size = 0`。这使框架 4 KiB 映射的 VMA 快照
错误地保存为零，并在首次访问时被判为 EOF 外页。

## 根因

1. mmap 文件页 fault 对完整 EOF 外页分配零页，而非向用户态投递 `SIGBUS`。
2. trap 层不能区分文件 EOF fault 与普通无效地址 fault，后者应为 `SIGSEGV`。
3. VMA 不保存映射建立时的文件长度；unlink 后继续按路径查询大小会失真。
4. lwext4 截断扩展后的 `stat` 长度存在延迟/失真，VFS 没有保存成功更新后的长度。

## 修复

- `MmapFile` 增加 `mapped_file_size`，用 mmap fd 的 `fstat()` 在创建或替换 VMA 时采样；
  该快照覆盖 unlink 后映射仍存活的语义。
- `MAP_FIXED` 覆盖已有映射时，`MemorySetInner::mprotect()` 可能将原 VMA 拆成前、中、后
  多段。所有承接新映射属性的目标段均调用
  `area.mmap_file.replace(file.clone(), offset)`（或对应新段的 `mmap_file.replace`），同时
  更新 `file`、`offset` 和 `mapped_file_size`；这避免保留被覆盖 VMA 的旧 EOF 快照。
- `mmap_file_page_beyond_eof()` 计算 `VMA offset + page offset`，页起点大于或等于快照 EOF
  时返回 true；尾部所在的部分页面仍可正常缺页并零填充。
- mmap 的 read/write fault handler 在分配页或查询共享页缓存之前执行 EOF 判断。
  `MemorySet` 向 trap 层暴露查询接口；普通 Load/Store/Fetch page fault 和 LoongArch64
  `PagePrivilegeIllegal` 在该条件下投递 `SIGBUS`，其他无法处理的 fault 仍投递 `SIGSEGV`。
- `Ext4Inode` 在成功 `truncate` 后保存 `known_size`，并在 `write_at` 后扩展该值；`size()`、
  `fstat()` 和文件页缓存读取该一致长度，避免成功的 `ftruncate` 被后续错误的 `st_size = 0`
  覆盖。

## 涉及文件

| 文件 | 修改 |
|---|---|
| `os/src/mm/map_area.rs` | 保存并复制 VMA 映射时的文件长度快照 |
| `os/src/mm/memory_set/{handle.rs,mmap_ops.rs}` | 提供 VMA EOF 查询、在 mmap fault 路径提前拒绝，并在 `MAP_FIXED` VMA 拆分时通过 `MmapFile::replace()` 刷新文件/偏移/长度快照 |
| `os/src/mm/page_fault_handler.rs` | 识别完整 EOF 外页，并在分配前阻止零页建立 |
| `os/src/trap/mod.rs` | 将文件 EOF fault 映射为 `SIGBUS`，其余失败维持 `SIGSEGV` |
| `os/src/fs/ext4_lw/inode.rs` | 保存 truncate/write 后一致的普通文件长度 |

## 验证

已执行：

```text
cargo fmt --manifest-path os/Cargo.toml --all
git diff --check
make log
timeout 120s make run > /tmp/mmap13-known-size.log 2>&1
```

默认 LoongArch64 `make log` 通过，只有既有 vendored `smoltcp` warning。运行日志显示：

```text
mmap13.c:60: TPASS: Received SIGBUS signal as expected
Summary:
passed   1
failed   0
broken   0
skipped  0
warnings 0
```

musl 与 glibc 两轮均为上述结果。调试过程确认框架共享映射不会再错误收到 SIGBUS；测试自身
半页文件映射记录为 `mapped_file_size=Some(2048)`，对第二页的写访问才触发被 handler 捕获的
`SIGBUS`。未运行 RISC-V QEMU；本轮没有修改架构专属页表格式，但 RISC-V 仍需后续回归。
