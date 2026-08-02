# lwext4 SMP P21.1：位置无关块设备前置

## 背景

《优化方案》的 P21 将 lwext4 SMP 化拆分为 block device、bcache、inode/open file、目录、分配器和
journal 等阶段。P21.1 的前提是每个底层 I/O 请求独立携带 LBA 和长度；否则两个 hart 的
`seek + read/write` 可交叉，造成读写落在错误扇区。

现有文件系统仍以 `EXT4_OP_LOCK` 串行化全部 lwext4 C API。该 gate 在 P21.2 的并发 bcache、P21.3
的 inode 只读同步和相应回归完成前不能撤退。

## 现象

旧 `Disk` 保存 `block_id` 与 `offset` 作为可变 cursor。`Ext4BlockWrapper::dev_bread()` 和
`dev_bwrite()` 从 FFI `p_user` 裸指针构造 `&mut Disk`，先调用 `seek()` 修改这个共享 cursor，再调用
`read()` 或 `write()`。

这在当前全局 gate 下偶然安全；一旦不同 hart 可同时调用 C block callback，两个 request 的 seek 与传输
可交叉。同时从同一原始指针构造多个活跃的 `&mut Disk` 本身也不符合 Rust aliasing 规则。

## 根因

块设备适配把请求位置保存在设备对象中，而非请求参数中。它没有为非对齐 read-modify-write 建立独立的
submission 边界，也未检查从 C callback 返回的短传输。因此它不是后续让多个 lwext4 只读调用并发进入的
安全基础。

## 修复

- `KernelDevOp` 改为 `device_size/read_at/write_at/flush`。所有 I/O 通过共享 `&DevType` 发起；请求位置
  由 `offset` 参数明确携带。
- `Ext4BlockWrapper` 的 `dev_open` 从 `device_size()` 初始化容量，`bread/bwrite` 将 LBA 转为字节范围，检查
  乘法和 `usize` 转换溢出、空 buffer 指针及短传输。短读或短写统一返回 `EIO`，不再被误报为 C API 成功。
- Ya2yOS `Disk` 删除 `block_id/offset`。它以 `Mutex<BlockDeviceImpl>` 保护一个完整的按位置 request；对齐的
  多扇区 I/O 直接提交，非对齐 I/O 的 read-modify-write 在同一 submission mutex 内完成。该 mutex 只覆盖设备
  传输，不覆盖 inode、bcache 或 filesystem metadata。
- `Ext4BlockWrapper::sync()` 在 lwext4 cache flush 成功后调用底层 `flush()`；独立示例和公开 trait 文档同步改为
  位置无关契约。
- 默认启用 `lwext4-smp` feature 作为后续显式 context API 的开关。它当前不改变路径，也不放宽
  `EXT4_OP_LOCK`。

## 涉及文件

- `crates/lwext4_rust/src/blockdev.rs`
- `os/src/drivers/disk.rs`
- `os/src/fs/ext4_lw/sb.rs`
- `os/Cargo.toml`
- `crates/lwext4_rust/Cargo.toml`
- `crates/lwext4_rust/examples/`
- `crates/lwext4_rust/README.md`
- `crates/lwext4_rust/doc/rust-ext4-fs-support.md`

## 验证

已通过：

```text
git diff --check
make build-arch TARGET_ARCH=riscv64
make build-arch TARGET_ARCH=loongarch64
```

两次构建均完成用户态和内核 release 链路。输出只有已有 `smoltcp` 的 unused import/dead-code warning。

未执行 8 HART 随机 LBA、同 LBA 重叠写、设备错误注入或 BuildStorm。这些需要 P21.0 所述固定镜像与可比较的
块设备测试 oracle；当前 C bcache 也仍受全局 gate 保护，因此运行 BuildStorm 不能验证尚未启用的 C-side SMP。
完整 `cargo fmt --manifest-path os/Cargo.toml --all -- --check` 仍会报告本次未触及的
`crates/lwext4_rust/src/file.rs` 既有格式差异；本次修改的 Rust 文件已单独 `rustfmt --check`。

## 后续

P21.2 必须先为 `ext4_bcache` 建立 LBA index、single-flight `LOADING`、dirty/writeback pin 和锁外 I/O
状态机，并完成同/异 LBA、flush/evict、I/O 失败和泄漏回归；在此之前不得让不同 inode 的 C read/fstat 绕过
`EXT4_OP_LOCK`。之后才进入 P21.3 的 inode identity/open file 与只读路径撤退。
