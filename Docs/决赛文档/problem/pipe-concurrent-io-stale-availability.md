# pipe 并发 I/O 使用过期可用长度导致 panic

## 背景

BuildStorm 会同时运行多个 Cargo/rustc worker，并通过 pipe 传递输出和进程控制信息。多核下同一
pipe 可能有多个 reader 或 writer 并发进入 `Pipe::read()` / `Pipe::write()`。

## 现象

`server.ans` 出现 pipe 缓冲区 panic，GDB 记录将现场收敛到普通 pipe 数据路径。触发依赖多核并发，
单 reader / writer 或低负载运行不容易复现。

## 分析

旧实现第一次取得 `PipeRingBuffer` 锁，读取 `available_read()` 或 `available_write()`，随后释放锁；
实际 `read_byte()`、`read_into()`、`write_byte()` 或 `write_owned_bytes()` 前又重新取得锁。

两次临界区之间，另一 reader 可以消费掉先前观察到的字节，另一 writer 也可以占用先前观察到的
空间。因此第二个临界区使用的是过期长度：读路径可能在空队列上取字节，写路径可能超过当前
容量并触发 `pipe buffer overflow` 断言。

## 根因

pipe 的“检查可用长度”和“按该长度消费/写入”不是一个原子操作。共享锁保护了每次单独访问，
但没有保护由两次访问组成的 check-then-act 操作。

## 修复

`Pipe::read()` 和 `Pipe::write()` 现在在同一次 `inner_lock()` 临界区内完成：

- 端点关闭、空/满和 nonblocking 状态检查；
- 计算本次可读或可写长度；
- 实际移动数据并更新 pipe 字节数；
- 唤醒对端 waiter，随后释放锁。

阻塞路径仍在登记 waiter 后释放 pipe 锁，再调用调度器，不持锁睡眠。

## 涉及文件

- `os/src/fs/files/pipe/file_impl.rs`

## 验证

- `make build-arch TARGET_ARCH=riscv64`：通过。
- `make build-arch TARGET_ARCH=loongarch64`：通过。
- 修复后的 RISC-V BuildStorm 已通过 toolchain/minibuild 并从 Cargo `440/446` 推进到 `444/446`，
  当前日志未再次出现 pipe panic；完整 BuildStorm 结束标记尚未取得。

