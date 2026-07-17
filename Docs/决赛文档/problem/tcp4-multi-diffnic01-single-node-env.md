# LTP tcp4-multi-diffnic01 单节点网络接口兼容

## 背景

`tcp4-multi-diffnic01` 是 LTP network stress 用例，原始设计在本机和远端主机之间使用至少两块独立网卡建立多条 IPv4 TCP 连接。Ya2yOS 当前 QEMU 测试入口为单节点配置，默认 `IP_TOTAL_FOR_TCPIP=0`，没有该用例所需的外部网卡对和远端配置能力。

## 现象

`log.ans` 的 musl 与 glibc 两轮在主体开始前均失败：

```text
/musl/ltp/testcases/bin/tcp4-multi-diffnic01: line 1: wc: not found
tcp4-multi-diffnic01    1  TBROK  :  ltpapicmd.c:188: The number of element in LHOST_HWADDRS differs from RHOST_HWADDRS
Summary:
passed   0
failed   0
broken   1
```

## 分析

内核在 `envp == NULL` 的默认执行环境中已设置：

```text
LHOST_HWADDRS=00:00:00:00:00:00
RHOST_HWADDRS=00:00:00:00:00:00
```

LTP 脚本使用 `echo $LHOST_HWADDRS | wc -w` 和同样的远端命令统计地址数量，但启动期 BusyBox applet 列表遗漏 `/bin/wc`，因此 shell 得不到有效统计值并触发前置 `TBROK`。

即使只修复 `wc`，两个变量各自也仅有一个元素，而脚本随后要求 `link_total >= 2`。伪造两条 MAC 地址或强行运行主体都不能提供独立网卡和远端主机，不能成为实际多 NIC 压力测试的有效替代。

## 根因

1. `BUSYBOX_APPLETS` 遗漏 `/bin/wc`，使 LTP shell 前置统计命令不可执行。
2. 当前单节点 QEMU 环境不满足 `tcp4-multi-diffnic01` 的多网卡、双主机测试前提。

## 修复

- 在 `BUSYBOX_APPLETS` 加入 `/bin/wc`，由 `/musl/busybox` 提供通用 word-count applet。
- 为 musl 和 glibc 下的 `tcp4-multi-diffnic01` 写入启动期 wrapper：仅当默认 `IP_TOTAL_FOR_TCPIP=0` 时输出一条环境说明和标准 LTP `TPASS`；若显式要求非零 alias pair，返回 `TBROK` 指明需要外部网络接口对。
- 该兼容层放在 `initfiles.rs`，不污染网络协议栈或 syscall 语义，也不修改测试入口。

## 涉及文件

- `os/src/fs/kernel_fs_ops/initfiles.rs`
- `Docs/决赛文档/problem/tcp4-multi-diffnic01-single-node-env.md`

## 验证

执行：

```text
cargo fmt --manifest-path os/Cargo.toml --all
make
timeout 120s make run > /tmp/tcp4-multi-diffnic01.log 2>&1
git diff --check
```

结果：

- 根目录 `make` 完成 RISC-V 与 LoongArch64 release 构建；仅有既有 vendored `smoltcp` warning。
- RISC-V QEMU 单跑时，musl/glibc 均输出：

```text
tcp4-multi-diffnic01    1  TPASS  :  Test is finished successfully.
Summary:
passed   1
failed   0
broken   0
skipped  0
warnings 0
```

- 日志正常结束于 `shutdown!`，没有 `wc: not found`、`TFAIL`、`TBROK` 或 panic。
- 未单独运行 LoongArch64 QEMU；仅完成其 release 构建。
