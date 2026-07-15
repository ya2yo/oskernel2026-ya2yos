# LTP socket01 socket type errno 语义修复

## 背景

LTP `socket01` 校验 `socket(2)` 对非法 domain、socket type 与 protocol 组合返回的 Linux errno。当前 `initproc` 只运行该用例的 musl 与 glibc 轮次，便于缩小问题范围。

## 现象

原始 `log.ans` 的两轮结果均为 `passed 7 failed 2 broken 0 skipped 0 warnings 0`。失败断言为：

- `socket(PF_INET, 75, 0)` 期望 `EINVAL`，实际得到 `ESOCKTNOSUPPORT`；
- `socket(PF_INET, SOCK_RAW, 0)` 期望 `EPROTONOSUPPORT`，实际得到 `ESOCKTNOSUPPORT`。

其余七项，包括无效 domain、AF_UNIX datagram、TCP/UDP 正确及错误 protocol 组合，均为 `TPASS`。

## 分析

Linux 在选择协议族前先校验 socket type 是否位于 `1..SOCK_MAX`。因此数值 `75` 不属于 Linux 定义的 socket type，应直接返回 `EINVAL`。`SOCK_RAW` 则是一个有效类型；Ya2yOS 尚未提供 IPv4/IPv6 raw socket 协议实现，故应以 `EPROTONOSUPPORT` 表示该协议能力不可用。

原实现只在 `(domain, type)` 匹配失败后统一返回 `ESOCKTNOSUPPORT`，混淆了非法 type 与有效但未实现的 raw socket，导致两项 errno 均不符合 LTP/Linux 语义。

## 修复

- 在 `sys_socket()` 解析 `raw_ty` 后先拒绝 `0` 和大于等于 Linux `SOCK_MAX`（11）的 type，返回 `EINVAL`；
- 为 `AF_INET` 和 `AF_INET6` 的 `SOCK_RAW` 增加显式分支，返回 `EPROTONOSUPPORT`；
- 不引入 raw socket 实现，不改变现有 TCP、UDP、AF_UNIX 或其余未支持 socket type 的行为。

## 涉及文件

- `os/src/syscall/net/socket.rs`

## 验证

- `rustfmt --edition 2021 --check os/src/syscall/net/socket.rs` 通过。
- `make` 通过，完成 RISC-V 与 LoongArch64 release 构建；仅出现既有 vendored `smoltcp` warning。
- 维护者提供的最新 `log.ans` 显示 musl 与 glibc 的九项断言均为 `TPASS`，两轮 Summary 都是 `passed 9 failed 0 broken 0 skipped 0 warnings 0`，并正常 `shutdown!`。
- `FAIL LTP CASE socket01 : 0` 只是 musl 测试包装层输出的退出码，实际 LTP Summary 的 `failed` 为 0。
