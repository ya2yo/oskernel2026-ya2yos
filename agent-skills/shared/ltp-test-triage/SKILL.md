---
name: ltp-test-triage
description: >-
  排查 LTP 失败与 initproc 单测配置。LTP 相关内核修复后须写文档，见 kernel-change。
---

# LTP 测试排查

> 修通测例后的文档：[kernel-change](../kernel-change/SKILL.md) + [doc-writing](../doc-writing/SKILL.md)。复盘写入 `Docs/初赛文档/problem/`。

## 架构

```
initproc (user/src/bin/initproc.rs)
  └─ fork_and_run("/musl/ltp/testcases/bin", [test_name])
       └─ 子进程执行 sdcard 上的预编译 LTP 二进制
```

测试二进制在磁盘镜像内，不在源码树；源码参考 [linux-test-project/ltp](https://github.com/linux-test-project/ltp)。

## 快速单测

1. 在 `user/src/bin/ltp/filelist.rs` 找到用例索引
2. 设置 `LTP_TEST_START` 指向该索引，`LTP_TESTS_PER_GROUP = 1`
3. `make log && make run`，保存输出到 `log.ans`

```rust
// initproc.rs
const LTP_TEST_START: usize = 3;       // accept02 等在 filelist 中的偏移
const LTP_TESTS_PER_GROUP: usize = 1;
```

## 输出解读

| 输出 | 含义 |
|------|------|
| `TPASS` | 用例断言通过 |
| `TFAIL` | 用例断言失败（真正失败） |
| `TBROK` | 测试框架中断（环境/实现缺失） |
| `Summary: passed N, failed M` | LTP 自身统计 |
| `FAIL LTP CASE X : N` | **initproc 打印退出码 N，不是失败标签**；N=0 表示进程正常退出 |

## 黑名单 `LTP_BLACKLIST`

位置：`user/src/bin/initproc.rs`

常见跳过原因：

| 用例前缀 | 原因 |
|----------|------|
| `cgroup_fj_*` | 需带参数脚本入口，直接跑会卡死 |
| `clone0*` | clone 语义复杂 |
| `connect01` | 网络相关 |
| `clock_nanosleep*` | 定时器精度 |

**cgroup_fj 正确跑法**：通过脚本传参，例如 `cgroup_fj_function.sh cpuset`，不要直接 exec `cgroup_fj_proc`。

## 典型失败模式 → 查哪里

| 症状 | 优先检查 |
|------|----------|
| `accept` 阻塞/死循环 | `block_on` 强引用、`task/future/`、`net/tcp.rs` accept 路径 |
| `futex` panic / EINTR | `syscall/sync/futex.rs`，信号打断时 Waiter 清理 |
| `clone` 失败 | `syscall/task/clone.rs`，`CLONE_*` 标志，tls/stack |
| `setsockopt` 语义错误 | `syscall/net/opt.rs` 是否传播 `set_option` 错误（`?`） |
| `/tmp` ENOENT | ext4 目录创建、`lwext4_rust`、测试 cleanup 时序 |
| 网络 connect/accept | `os/src/net/`、`drivers/virtio/net.rs` |

## accept02 专项（CVE-2017-8890）

测试逻辑：
1. listener `MCAST_JOIN_GROUP` 加入组播
2. `accept()` 得新 fd
3. 对新 fd `MCAST_LEAVE_GROUP` **应失败** `EADDRNOTAVAIL`（组播未复制）

通过标志：`TPASS: Multicast group was not copied: EADDRNOTAVAIL`

常见误判：
- `setsockopt` 返回 0 但应返回 -1 → 检查 `opt.rs` 末尾是否吞掉 `set_option` 错误
- 末尾 `ext4_fopen ... ENOENT` → cleanup 噪音，不影响 TPASS

## 调试流程

```
1. 确认 TPASS/TFAIL（不是 FAIL LTP CASE 字样）
2. rg -a 搜 log.ans：syscall 序列、panic、ERROR
3. 对照 LTP 源码（GitHub）理解断言
4. 映射到内核模块（syscall → net/fs/task）
5. 单测复现 → 修 → 再跑
```

## 相关文档

- `Docs/初赛文档/problem/` — 历史 bug 复盘（[索引](../../Docs/初赛文档/problem/README.md)）
- [kernel-change](../kernel-change/SKILL.md) — 修通后写文档
- [debug-playbook](../debug-playbook/SKILL.md) — 通用调试模式
