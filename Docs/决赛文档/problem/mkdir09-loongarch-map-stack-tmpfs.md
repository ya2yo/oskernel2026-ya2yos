# mkdir09 LoongArch mmap 栈缺页与 tmpfs 挂载隔离

## 背景

LTP `mkdir09` 会在每个支持的文件系统上创建多个 pthread，线程并发执行三类目录操作：

- 对已存在目录执行 `mkdir(..., 07770)`，期望 `EEXIST`。
- 对不存在目录执行 `rmdir()`，期望 `ENOENT`。
- 创建并删除线程私有目录，期望成功。

该用例使用 `.all_filesystems = 1`，会先后在 `ext2`、`tmpfs` 等文件系统上复用同一个 `mntpoint`。

## 现象

原始 `log.ans` 在 LoongArch64 单跑 glibc `mkdir09` 时卡在 ext2 阶段：

```text
tst_test.c:1120: TINFO: Mounting /dev/loop0 to /tmp/LTP_.../mntpoint fstyp=ext2 flags=0
tst_memutils.c:152: TINFO: oom_score_adj does not exist, skipping the adjustment
[WARN] ... PagePrivilegeIllegal in application, bad addr = 0x2a21c404b0, bad instruction = 0x2a232dacb8, sending SIGSEGV.
```

配套 `ana.ans` 显示失败前正在创建 pthread：

```text
Mmap ret = 180955127808
Mprotect ret = 0
Clone3 ret = 24
```

修复该 fault 后，测试继续推进到 tmpfs 阶段，但暴露新的失败：

```text
tst_test.c:1120: TINFO: Mounting ltp-tmpfs to /tmp/LTP_.../mntpoint fstyp=tmpfs flags=0
mkdir09.c:134: TBROK: mkdir(mntpoint/X.0, 7770) failed: EEXIST (17)

Summary:
passed   6
failed   0
broken   1
```

## 分析

### LoongArch `PagePrivilegeIllegal`

临时日志确认 glibc pthread 栈的创建流程如下：

```text
mmap(addr=0, len=0x801000, prot=0, flags=MAP_PRIVATE|MAP_ANONYMOUS|MAP_STACK)
mprotect(base + 0x1000, 0x800000, PROT_READ|PROT_WRITE)
clone3(stack=base, stack_size=0x7ffa60, tls=...)
```

第三个线程栈触发 fault 时，访问地址 `0x2a21c404b0` 位于刚刚 `mprotect()` 改成 RW 的 `MAP_STACK` 懒分配 VMA 内，不在 guard page 内。该页尚未实际分配物理页，应该走 lazy allocation。

RISC-V/通用路径中的 Load/Store page fault 会进入 `MemorySetInner::handle_page_fault()`，但 LoongArch 这次把未映射访问报告为 `PagePrivilegeIllegal`，原 `trap` 分支直接发送 `SIGSEGV`，没有机会触发 `Stack` 区域的懒分配。

同时，`Stack/Brk` 懒分配原本不检查 VMA 权限，若直接把所有 `PagePrivilegeIllegal` 都交给 lazy handler，会把 `PROT_NONE` guard page 也错误映射出来。因此需要同时补权限判断。

### tmpfs 轮次污染

LTP `run_tcases_per_fs()` 每轮执行：

1. `prepare_device()` mount 当前文件系统到同一个 `mntpoint`。
2. fork 子进程运行测试。
3. `tst_umount(mntpoint)`。

Ya2yOS 当前 mount 实现只维护 `MNT_TABLE`，文件系统操作仍落到同一个底层 ext4 目录。ext2 轮次中 `mkdir09` 创建的 `mntpoint/X.*` 实际留在底层目录中；后续 tmpfs mount 只是记录 mount table，不能像 Linux tmpfs 一样提供空根目录视图，所以 tmpfs setup 再次创建 `mntpoint/X.0` 时得到 `EEXIST`。

## 根因

1. LoongArch `PagePrivilegeIllegal` 分支没有先尝试统一缺页处理，导致 `MAP_STACK` 懒分配页被误判为用户 SIGSEGV。
2. `Stack/Brk` 懒分配缺少 VMA 权限检查，不能安全承接 LoongArch 的 `PagePrivilegeIllegal`。
3. 简化 mount 模型没有真实 per-mount superblock/目录树隔离，tmpfs mount 不能隐藏或替换上一个文件系统轮次留下的目录内容。

## 修复

涉及文件：

- `os/src/trap/mod.rs`
- `os/src/mm/memory_set/mmap_ops.rs`
- `os/src/syscall/fs/mount.rs`

修复内容：

1. `PagePrivilegeIllegal` 先解析 `stval`，调用 `memory_set.handle_page_fault()`；只有 lazy/COW 都无法处理时才发送 `SIGSEGV`。
2. `MemorySetInner::handle_not_present_page_fault()` 为 `PagePrivilegeIllegal` 增加 mmap/stack/brk 权限判断：
   - mmap 区域有 `R/X` 时走 read fault 装页，否则有 `W` 时走 write fault。
   - brk/stack 区域只有在 VMA 具备 `R/W/X` 任一权限时才允许 lazy allocation，避免 `PROT_NONE` guard page 被映射。
3. `sys_mount()` 在 `tmpfs` 非 remount 挂载前清空挂载点目录内容，用最小代价模拟 tmpfs 空根目录，避免 LTP `all_filesystems` 轮次间状态泄漏。

## 验证

已执行：

```text
cargo fmt --manifest-path os/Cargo.toml
make TARGET_ARCH=loongarch64
timeout 120s make run
```

构建通过。`make run` 当前入口为 LoongArch64 glibc 单跑 `mkdir09`，关键结果：

```text
mkdir09.c:47: TPASS: [0] create dirs that already exist
mkdir09.c:67: TPASS: [1] remove dirs that do not exist
mkdir09.c:93: TPASS: [2] create/remove dirs
...
Summary:
passed   12
failed   0
broken   0
skipped  0
warnings 0
shutdown!
```

未再出现原始 `PagePrivilegeIllegal bad addr = 0x2a21c404b0`，tmpfs 阶段也不再出现 `mkdir(mntpoint/X.0) failed: EEXIST`。

未执行 RISC-V 验证；本次失败日志、修复触发路径和验证均集中在当前 LoongArch64 `mkdir09` 单测入口。
