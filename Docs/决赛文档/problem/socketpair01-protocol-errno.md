# LTP socketpair01 协议 errno 与用户指针语义修复

## 背景

LTP `socketpair01` 覆盖 `socketpair(2)` 的 domain、type、protocol 和用户输出指针错误路径。该用例分别以 musl 和 glibc 运行。

## 现象

原始 `log.ans` 中两轮均为 `passed 4 failed 6 broken 0 skipped 0 warnings 0`。六项失败都返回了 `EAFNOSUPPORT`：

- `socketpair(PF_INET, 75, 0, fds)` 应为 `EINVAL`；
- `socketpair(PF_INET, SOCK_RAW, 0, fds)` 应为 `EPROTONOSUPPORT`；
- `socketpair(PF_INET, SOCK_DGRAM, IPPROTO_UDP, fds)` 和 TCP stream 应为 `EOPNOTSUPP`；
- `SOCK_DGRAM/IPPROTO_TCP` 与 `SOCK_STREAM/IPPROTO_ICMP` 应为 `EPROTONOSUPPORT`。

修正上述 errno 后，首次 RISC-V 回归继续暴露 `sv=(int *)7` 错误成功，随后 LTP 尝试关闭没有写回的 `fds[]` 并报 `TBROK: close(-1) failed: EBADF`。

## 分析

Linux `__sys_socketpair()` 会先按 type/protocol 创建两个协议 socket，再调用协议族的 `socketpair` 操作。对于 AF_INET，TCP/UDP 对应组合能创建 socket，但 inet 协议没有 pair 操作，因而返回 `EOPNOTSUPP`；type/protocol 不匹配则在协议选择阶段返回 `EPROTONOSUPPORT`。

原实现把 `domain != AF_UNIX` 直接映射成 `EAFNOSUPPORT`，跳过了有效 AF_INET 的 type/protocol 分层。另一个问题在 RISC-V user-copy：`copy_to_user()` 只有 LoongArch64 构建才检查用户 VMA 覆盖与写权限；RISC-V 对地址 7 会直接进入按需缺页处理，使未映射低地址错误成为可写页。

## 修复

- 在 `sys_socketpair()` 的 domain 分发前校验 type 为 Linux `1..SOCK_MAX`，非法 type 返回 `EINVAL`；
- 对 AF_INET/AF_INET6 的 TCP、UDP（及已有 SCTP stream 兼容）返回 `EOPNOTSUPP`，表示 socket 可创建但不支持 socketpair；对 raw socket 与 TCP/UDP type-protocol 不匹配返回 `EPROTONOSUPPORT`；
- 将既有 `user_range_has_perm()` 从 LoongArch64 专用逻辑推广到 RISC-V 的 `copy_from_user()`、`copy_to_user()` 和 `probe_user_write()`。合法 VMA 仍可触发延迟分配/COW；不属于用户 VMA 的读写在页故障前返回 `EFAULT`；
- AF_UNIX pair 创建、fd 安装、非阻塞设置及 copy 失败后的 fd 清理保持不变。

## 涉及文件

- `os/src/syscall/net/socket.rs`
- `os/src/mm/translate.rs`

## 验证

- `rustfmt --edition 2021 os/src/mm/translate.rs os/src/syscall/net/socket.rs` 通过。
- `make` 通过，完成 RISC-V 与 LoongArch64 release 构建；仅出现既有 vendored `smoltcp` warning。
- `timeout 120s make TARGET_ARCH=riscv64 run > /tmp/socketpair01-riscv64.log 2>&1` 通过。musl 与 glibc 的十项断言均为 `TPASS`，两轮 Summary 都是 `passed 10 failed 0 broken 0 skipped 0 warnings 0`，最后正常 `shutdown!`。
- `FAIL LTP CASE socketpair01 : 0` 是 musl 测试包装层输出的退出码，不代表 LTP 失败。
