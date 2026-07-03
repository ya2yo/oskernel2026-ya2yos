# bind01: bind 非本地地址与 AF_UNIX 路径前缀语义修复

## 背景

LTP `bind01` 覆盖 `bind(2)` 的基础错误语义，包括无效长度、非 socket fd、`INADDR_ANY`、AF_UNIX 地址族、非本地 IPv4 地址、无效 fd，以及 AF_UNIX pathname 前缀不是目录等场景。

本次回归集中在两个路径：

- IPv4 socket 绑定非本机地址时应返回 `EADDRNOTAVAIL`。
- AF_UNIX pathname socket 绑定到 `/path/file/child` 这类“路径前缀中某个分量不是目录”的地址时应返回 `ENOTDIR`。

## 现象

修复前 `log.ans` 中 `bind01` 有 2 个失败：

```text
bind01.c:60: TFAIL: non-local address succeeded
bind01.c:60: TFAIL: a component of addr prefix is not a directory succeeded

Summary:
passed   5
failed   2
broken   0
skipped  0
warnings 0
```

其余基础场景已经通过，包括无效 `sockaddr` 长度、非 socket fd、`INADDR_ANY` 和非法 fd。

## 分析

### IPv4 非本地地址

TCP/UDP `bind()` 原实现会把用户传入地址转成 `IpListenEndpoint`，再用 `get_service().device_mask_for(&endpoint)` 选择可用设备。`device_mask_for()` 基于路由表查找地址，而当前网络初始化添加了默认路由：

```rust
router.add_rule(Rule::new(
    Ipv4Cidr::new(Ipv4Address::UNSPECIFIED, 0).into(),
    Some(GATEWAY.parse().expect("Invalid gateway address")),
    eth0_dev,
    eth0_ip.address().into(),
));
```

因此非本地地址也能通过默认路由匹配到设备，`bind()` 被错误接受。但 Linux `bind(2)` 对具体本地地址的要求不是“有路由可达”，而是“该地址属于本机接口”，否则应返回 `EADDRNOTAVAIL`。只有 wildcard 地址 `0.0.0.0` 允许绑定所有本机地址。

### AF_UNIX pathname 前缀

AF_UNIX socket 原实现只把 pathname 地址登记到内存表：

```rust
if binds.contains_key(&local_addr) {
    return Err(SysErrNo::EADDRINUSE);
}
binds.insert(local_addr.clone(), self.inner.clone());
```

它没有校验 pathname 所在的文件系统前缀。这样 `/tmp/file/child` 这种路径即使中间的 `file` 是普通文件，也会被当成普通字符串 key 接受，导致 LTP 的 `ENOTDIR` 场景误成功。

当前 Ya2yOS 的 AF_UNIX pathname socket 仍主要由 `UNIX_BINDS` 内存表管理，并未在 VFS 中创建 socket inode。本次修复只补 Linux 可见错误语义：绑定 pathname 前先验证父目录路径，复用现有 VFS `open()` 的 `ENOTDIR/ENOENT` 行为。

## 根因

- IPv4 `bind()` 混淆了“路由可达地址”和“本机接口地址”，缺少本地地址归属检查。
- AF_UNIX pathname `bind()` 把路径仅当作内存表 key，没有校验路径前缀是否存在且是否为目录。

## 修复

### 本地地址检查

在 `os/src/net/mod.rs` 新增 `check_local_bind_address()`：

- `0.0.0.0` / unspecified 地址直接允许。
- 精确匹配 `iface.ip_addrs()` 中的接口地址时允许。
- 对 loopback 接口保留 `127/8` 语义：接口配置为 `127.0.0.1/8` 时，`127.x.x.x` 视为本地 loopback。
- 其他地址返回 `EADDRNOTAVAIL`。

TCP 和 UDP `bind()` 在分配 ephemeral port 后、检查特权端口前调用该 helper。

### AF_UNIX pathname 父目录检查

在 `os/src/net/unix.rs` 中新增 `check_path_parent()`：

- 根据当前进程 cwd 把 pathname 转成绝对路径。
- 用 `rsplit_once()` 取父目录。
- 调用 `open(parent_path, O_RDONLY | O_DIRECTORY, NONE_MODE)` 复用 VFS 校验。
- 父路径中某个分量不是目录时自然返回 `ENOTDIR`。

检查通过后才进入 `UNIX_BINDS` 内存表登记。

## 涉及文件

| 文件 | 修改 |
|------|------|
| `os/src/net/mod.rs` | 新增 `check_local_bind_address()`，按接口地址判断 IPv4/IPv6 bind 地址是否本地 |
| `os/src/net/tcp.rs` | TCP `bind()` 调用本地地址检查，非本地地址返回 `EADDRNOTAVAIL` |
| `os/src/net/udp.rs` | UDP `bind()` 调用本地地址检查，非本地地址返回 `EADDRNOTAVAIL` |
| `os/src/net/unix.rs` | AF_UNIX pathname bind 前校验父目录，非法路径前缀返回 VFS errno |

## 验证

已执行：

```text
make
```

结果：默认 RISC-V 构建通过，仅有既有 warning。

AI 曾尝试在沙箱内运行：

```text
TMPDIR=/tmp timeout 120s make run > /tmp/bind01-fix.log 2>&1
```

QEMU 仍尝试在只读 `/var/tmp` 创建临时文件，未能启动：

```text
qemu-system-riscv64: ... Could not open temporary file '/var/tmp/...': Read-only file system
```

随后用户在可运行环境中完成验证，最新 `log.ans` 显示 `bind01` 核心断言全部通过：

```text
bind01.c:60: TPASS: invalid salen : EINVAL (22)
bind01.c:60: TPASS: invalid socket : ENOTSOCK (88)
bind01.c:63: TPASS: INADDR_ANYPORT passed
bind01.c:60: TPASS: UNIX-domain of current directory : EAFNOSUPPORT (97)
bind01.c:60: TPASS: non-local address : EADDRNOTAVAIL (99)
bind01.c:60: TPASS: sockfd is not a valid file descriptor : EBADF (9)
bind01.c:60: TPASS: a component of addr prefix is not a directory : ENOTDIR (20)

Summary:
passed   7
failed   0
broken   0
skipped  0
warnings 0
```

日志中仍有包装器行：

```text
FAIL LTP CASE bind01 : 10
```

但 LTP 本体 summary 为 `passed 7 failed 0 broken 0`，本仓库判读 LTP 结果时以 `TPASS/TFAIL/TBROK/Summary` 为准。

未执行 `TARGET_ARCH=loongarch64` 验证；本次复现与验证基于当前默认 RISC-V 配置和用户提供的最新 `log.ans`。
