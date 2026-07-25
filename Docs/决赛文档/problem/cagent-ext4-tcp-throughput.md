# CAgent 全量并发 EXT4/TCP 吞吐优化

## 背景

决赛 CAgent 同时启动 10 个 `agent_lite` 客户端和一个 loopback HTTP server。评测机上各 case 可全部通过但耗时达到 41--56 秒，远慢于 Linux；BuildStorm 也同样受并发文件访问影响。

## 现象

`log.ans` 最后一个 `shutdown!` 后的 RISC-V perf 快照覆盖 9.345 秒 CAgent 全量执行：

- EXT4 全局操作锁取得 51,775 次，累计等待 290,340,683 tick（10 MHz 时钟下约 29.034 秒），累计持锁 73,338,475 tick（约 7.334 秒）。
- 文件页缓存有 9,719 次命中、232 次缺页和 10,037 次文件映射 page fault。
- `read` syscall 边界累计为 38.187 秒，但仅实际运行区间为 0.376 秒，说明主要时间被串行锁等待放大，而不是读数据本身。
- `accept`、`recv` 的边界时间同样包含任务睡眠；对应 active 统计很小，不能把它们误判为 CPU 热点。

## 分析

lwext4 的路径式 API 与共享块缓存确实需要 `EXT4_OP_LOCK` 串行化，但两个高频只读查询也错误经过该锁：

1. 文件映射每次缺页在页缓存命中前调用 `inode.path()`，而旧实现只读取路径字符串也获取全局 EXT4 锁。
2. mmap EOF 判断反复调用 `inode.size()`；文件大小已经确认后仍取得全局锁。
3. 全局 `FilePageCache` 使用独占 `Mutex`，所有缓存命中在 8 hart 上互相阻塞。

网络侧另有独立的无效工作：TCP `send`/`recv` 每次先轮询整个 router/socket set，router 为监听 SYN 检测把 RX 队列全部 dequeue、分配 `Vec`、复制并重新 enqueue。该路径会放大全量 CAgent 的 loopback HTTP 并发开销。

## 修复

- `Ext4Inode` 保存由 `RwLock` 保护的 VFS 路径镜像；成功 rename 和 alias recovery 时同步更新。`path()` 不再进入 lwext4 全局锁。
- 已确认的普通文件大小使用原子缓存。首次查询仍在 EXT4 锁内完成；写入、截断和 `read_all` 以 release 更新，后续 mmap EOF 检查无需锁。
- `FilePageCache` 改为 `RwLock<BTreeMap<...>>`：缓存命中使用共享读锁，加载/失效仍用写锁并保留插入前二次检查。
- TCP 收发先检查本地 socket buffer，只在 `EAGAIN` 时轮询协议栈；发送直接从 `UserBuffer` 拷贝到 smoltcp TX buffer，删除临时 `Vec` 与二次复制。
- router 改为在 smoltcp 消费前 peek 当前 ingress 包，仅检查该包的 SYN，删除整队列复制。

真实 lwext4 调用、TCP 阻塞语义、FIN/`MSG_PEEK` 语义和页缓存重复加载的二次检查均保留。

## 涉及文件

- `os/src/fs/ext4_lw/inode.rs`
- `os/src/fs/page_cache.rs`
- `os/src/net/tcp.rs`
- `os/src/net/router.rs`
- `os/src/net/service.rs`
- `os/src/task/processor.rs`
- `os/src/utils/perf.rs`

## 验证

执行并通过：

```text
cargo fmt --manifest-path os/Cargo.toml
make perf TARGET_ARCH=riscv64
make perf TARGET_ARCH=loongarch64
git diff --check
```

本机变更后的全量 QEMU CAgent 在启动时被中断，未留下可比较的 wall-clock A/B 样本；不以本机数据声明具体加速比。维护者已反馈评测性能提升明显，后续应以相同评测机配置重新采集 `shutdown!` 后的 EXT4 锁等待、文件页缓存命中和十项 CAgent wall-clock。
