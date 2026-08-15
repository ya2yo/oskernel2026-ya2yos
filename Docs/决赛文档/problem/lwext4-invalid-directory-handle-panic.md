# lwext4 目录句柄打开失败后的空指针 panic

## 背景

维护者提供的 `log.ans` 在 LTP musl `getcwd03` 执行期间先报告
`readlink(...)=EINVAL`，随后内核发生 RISC-V `LoadPageFault`：

```text
stval: 0x4a8
sepc : ext4_fs_rwlock_get_kind
```

`0x4a8` 是接近空地址的访问，不能将其解释为用户态 `readlink` 缓冲区错误。

## 现象

对 `getcwd03` 进行定向重放时，`readlink()` 仍返回 `EINVAL`，随后 C 侧
lwext4 锁分类回调读取非法锁地址并 panic。`ext4_fs_rwlock_get_kind()` 读取锁对象
`kind` 字段时的偏移为 8，因此 `stval=0x4a8` 对应传入的锁指针为 `0x4a0`。

`0x4a0` 恰好是把空 `struct ext4_mountpoint *` 代入其 `fs.inode_locks[]`
成员计算后得到的地址，表明 C 代码在空 mount-point 上计算了 inode 锁，而非正常
挂载状态损坏。

## 分析

`Ext4File::read_dir_from()` 将 `ext4_dir` 零初始化后调用 `ext4_dir_open()`，但忽略
该函数的错误返回。路径查找失败时，`ext4_dir_open()` 让 `dir->f.mp` 保持空值；包装层
仍调用 `ext4_dir_entry_next()`。原 C 实现立即执行 `EXT4_NS_READ_LOCK(dir->f.mp)` 和
`EXT4_INODE_READ_LOCK(dir->f.mp, ...)`，经已注册的 Rust 锁回调进入
`ext4_fs_rwlock_get_kind()` 后解引用低地址。

同一 C 库中原先依赖 `ext4_assert(file && file->mp)` 的 `ext4_fread()` 也有相同风险。
当前构建配置下断言可能被编译为空操作，不能作为运行时参数校验。

## 根因

lwext4 的 Rust 包装层没有传播目录打开失败，且 C 迭代器入口没有验证目录文件句柄。
二者组合使一次可恢复的 lookup 错误转化为内核空指针 panic。`ext4_fread()` 对空
mount-point 的断言依赖是同类不安全边界。

## 修复

- `crates/lwext4_rust/src/file.rs`：检查 `ext4_dir_open()` 的返回值；失败时释放路径
  字符串并把 errno 返回给上层，不进入目录迭代和关闭路径。
- `crates/lwext4_rust/c/lwext4/src/ext4.c`：`ext4_dir_entry_next()` 在获取 namespace/inode
  锁前检查 `dir` 和 `dir->f.mp`，非法描述符直接返回 `NULL`。
- `crates/lwext4_rust/c/lwext4/src/ext4.c`：`ext4_fread()` 显式检查 `file` 与 `file->mp`，
  返回 `EINVAL`，不再依赖可能失效的断言。

## 涉及文件

- `crates/lwext4_rust/src/file.rs`
- `crates/lwext4_rust/c/lwext4/src/ext4.c`

## 验证

- 定向 RISC-V QEMU 重放 `getcwd03`：仍观察到 `readlink(...)=EINVAL` 和 LTP `TBROK`，但
  不再出现 `KERNEL PANIC`，测例随后输出组结束标记并执行 `shutdown!`。
- `make TARGET_ARCH=riscv64` 通过。
- `make TARGET_ARCH=loongarch64` 通过。根 Makefile 在该调用中顺序完成两架构 release
  构建；仅出现既有 `smoltcp` unused import 和 `initproc` dead-code warning。

本次没有修复 `getcwd03` 的 `readlink` 返回 `EINVAL` 语义，该问题仍需单独定位。
