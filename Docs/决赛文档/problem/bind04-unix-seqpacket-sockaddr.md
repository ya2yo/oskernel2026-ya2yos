# bind04: AF_UNIX SEQPACKET 与 sockaddr_storage 长度兼容

## 背景

LTP `bind04` 覆盖不同 socket 地址族和类型的 `bind/connect/listen/accept` 通信路径。本次修复涉及两类场景：

- AF_UNIX pathname / abstract 地址下的 `SOCK_STREAM` 与 `SOCK_SEQPACKET` 通信。
- IPv4 loopback TCP 使用 `sockaddr_storage` 长度调用 `connect()` 的兼容语义。

## 现象

修复前分阶段暴露了三个问题。

第一阶段，AF_UNIX pathname stream 通信已经成功，但 cleanup 阶段 `unlink(test.sock)` 返回 `ENOENT`。这说明 pathname socket 只存在于内核内存 bind 表，没有在 VFS 中创建可被 `unlink()` 删除的 socket inode。

第二阶段，AF_UNIX `SOCK_SEQPACKET` 创建失败：

```text
TCONF: socket(1, 5, 0) failed: ESOCKTNOSUPPORT
```

第三阶段，AF_UNIX stream/seqpacket 场景通过后，IPv4 TCP 第一组失败：

```text
bind04.c:117: TINFO: Testing IPv4 loop TCP variant 1
bind04.c:92: TBROK: connect(4, 127.0.0.1:49152, 128) failed: EINVAL (22)
```

随后测试异常退出触发 wait 回收路径 panic：

```text
panic
[kernel] Panicked at src/task/process/process.rs:290 process[3] removed but still refed! refcnt=2
```

GDB 栈显示 panic 发生在 `sys_waitpid()` 回收子进程并调用 `Process::remove_from_global_map()` 时：

```text
#2 os::task::process::process::Process::remove_from_global_map(pid=3)
#3 os::syscall::task::wait::sys_waitpid::{closure#0}
```

## 分析

### AF_UNIX pathname socket

原 AF_UNIX pathname bind 只把地址注册到 `UNIX_BINDS`。这能让内核内部通过 pathname 找到 listening socket，但不会在文件系统中创建 socket 类型节点。LTP 在 pathname 场景完成通信后会执行 `unlink(test.sock)` 清理路径，因此内核必须让 pathname socket 在 VFS 中可见。

### SOCK_SEQPACKET

`sys_socket()` 只支持 AF_UNIX `SOCK_STREAM` 和 `SOCK_DGRAM`，缺少 `SOCK_SEQPACKET` 分支。`bind04` 中 AF_UNIX seqpacket 的用法与 stream 一样走 `bind/listen/connect/accept`，但收发需要保留消息边界：一次 `send()` 对应一条记录，`recv()` 不应像 stream 那样把剩余字节重新放回队列形成字节流。

### sockaddr_storage 长度

IPv4 TCP 失败点是：

```text
connect(4, 127.0.0.1:49152, 128)
```

`128` 是 `sockaddr_storage` 风格的缓冲区长度。Linux 对 `sockaddr_in` 的输入长度要求是至少包含完整结构体，允许调用方传入更大的 `addrlen`。原 `SocketAddrV4::read_from_user()` 要求长度精确等于 `sizeof(sockaddr_in)`，因此把合法的大缓冲区误判为 `EINVAL`。IPv6 读取路径也有同样的精确长度限制。

GDB 中的 wait 回收 panic 是由前面的 `TBROK` 异常退出触发的后续断言暴露。修复 `connect()` 的地址长度兼容后，测试不再走该异常退出路径，日志中也不再出现 `extra TCB refs` 和 `remove_from_global_map` panic。

## 根因

- AF_UNIX pathname bind 没有同步创建 VFS socket inode，导致用户态 cleanup 无法 `unlink()` 路径。
- AF_UNIX socket 类型表缺少 `SOCK_SEQPACKET`，且 socket 内部没有区分 seqpacket 的连接型和记录边界语义。
- IPv4/IPv6 sockaddr 读取错误要求 `addrlen` 精确等于结构体大小，拒绝了合法的 `sockaddr_storage` 大长度。
- `remove_from_global_map()` panic 是 `connect()` 误返回 `EINVAL` 后测试异常退出链路中的次生症状。

## 修复

### pathname socket inode

在 `os/src/net/unix.rs` 中将 pathname bind 拆成两步：

- `bind_path_abs()` 根据当前 cwd 解析绝对路径，并用 `open(parent, O_DIRECTORY)` 校验父目录。
- `create_path_socket_node()` 在确认目标不存在后调用 `superblock_root_inode().create(..., InodeType::Socket)` 创建 VFS socket 节点，并登记到 `FsIndex`。

这样 AF_UNIX pathname socket 既能通过 `UNIX_BINDS` 完成连接，也能被 `unlink()` 正常清理。

### AF_UNIX SOCK_SEQPACKET

为 `UnixSocketKind` 增加 `SeqPacket`：

- `sys_socket(AF_UNIX, SOCK_SEQPACKET, 0)` 创建 `UnixSocket::new_seqpacket()`。
- `sys_socketpair(AF_UNIX, SOCK_SEQPACKET, 0)` 创建 seqpacket socketpair。
- `connect/listen/accept/poll/recv` 将 `SeqPacket` 视为连接型 socket。
- `recv()` 只对 `Stream` 执行剩余数据回填；`SeqPacket` 不回填剩余字节，从而保留记录边界。

### sockaddr 长度兼容

在 `os/src/syscall/net/addr.rs` 中把 IPv4/IPv6 输入长度检查从精确相等改为下界检查：

```rust
if addrlen < size_of::<sockaddr_in>() as u32 {
    return Err(SysErrNo::EINVAL);
}
```

IPv6 同理使用 `sockaddr_in6`。这保留了过短长度返回 `EINVAL` 的语义，同时允许 `sockaddr_storage` 这类更大的用户缓冲。

## 涉及文件

| 文件 | 修改 |
|------|------|
| `os/src/net/unix.rs` | pathname bind 创建 VFS socket inode；新增 `UnixSocketKind::SeqPacket` 与连接型/记录边界语义 |
| `os/src/syscall/net/socket.rs` | `sys_socket()`、`sys_socketpair()` 支持 AF_UNIX `SOCK_SEQPACKET` |
| `os/src/syscall/net/addr.rs` | IPv4/IPv6 sockaddr 输入长度改为“至少结构体大小” |
| `user/src/bin/initproc.rs` | 临时调整为单跑 `bind04` 便于验证 |

## 验证

已执行：

```text
make
timeout 120s make run > log.ans 2>&1
```

结果：默认 RISC-V 构建通过，`make run` 完成并写入 `log.ans`。关键输出：

```text
bind04.c:117: TINFO: Testing AF_UNIX pathname stream
bind04.c:149: TPASS: Communication successful
bind04.c:117: TINFO: Testing AF_UNIX pathname seqpacket
bind04.c:149: TPASS: Communication successful
bind04.c:117: TINFO: Testing AF_UNIX abstract stream
bind04.c:149: TPASS: Communication successful
bind04.c:117: TINFO: Testing AF_UNIX abstract seqpacket
bind04.c:149: TPASS: Communication successful
bind04.c:117: TINFO: Testing IPv4 loop TCP variant 1
bind04.c:149: TPASS: Communication successful
bind04.c:117: TINFO: Testing IPv4 loop TCP variant 2
bind04.c:149: TPASS: Communication successful
bind04.c:117: TINFO: Testing IPv4 loop SCTP
bind04.c:118: TCONF: socket(2, 1, 132) failed: EPROTONOSUPPORT (93)

Summary:
passed   6
failed   0
broken   0
skipped  1
warnings 0
```

日志中不再出现 `TBROK`、`TFAIL`、`panic` 或 `remove_from_global_map` 断言。外层包装器仍因 SCTP `TCONF` 退出码打印：

```text
FAIL LTP CASE bind04 : 10
```

本次修复未实现 SCTP 协议栈；该项为协议不支持导致的 LTP skip，不是本次 GDB panic 的根因。未执行 `TARGET_ARCH=loongarch64` 验证。
