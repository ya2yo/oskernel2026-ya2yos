# epoll_create02 RISC-V musl libc 包装语义

## 背景

LTP `epoll_create02` 校验旧接口 `epoll_create(size)` 的 Linux 兼容语义：当 `size <= 0` 时应失败并设置 `errno=EINVAL`。

在 RISC-V/LoongArch64 这类新架构上，Linux syscall 表只保留 `epoll_create1`，没有旧的 `__NR_epoll_create`。因此 LTP 的 `syscall __NR_epoll_create` 变体会 `TCONF`，真实需要修复的是 libc `epoll_create()` 变体。

## 现象

用户提供的 `log.ans` 中，RISC-V musl 单跑 `epoll_create02` 时出现：

```text
epoll_create.h:32: TINFO: Testing variant: libc epoll_create()
epoll_create02.c:32: TFAIL: epoll_create(0) invalid retval 3: SUCCESS (0)
epoll_create02.c:32: TFAIL: epoll_create(-1) invalid retval 4: SUCCESS (0)
```

内核日志同时显示两次都进入 `sys_epoll_create1` 并创建了 fd：

```text
[sys_epoll_create1] flags=(empty)
[sys_epoll_create1] created epoll fd=3
```

## 分析

内核 `sys_epoll_create1(flags)` 本身按 `epoll_create1(2)` 语义处理 flags：`flags=0` 是合法值，不能为了旧 `epoll_create(0)` 把 syscall 20 改成失败，否则会破坏合法的 `epoll_create1(0)` 和 `epoll_create1_01`。

从预赛镜像只读提取 RISC-V `/musl/lib/libc.so` 后，符号和反汇编显示：

```text
00000000000215cc g    DF .text  0000000000000028 epoll_create1
00000000000215f4 g    DF .text  0000000000000008 epoll_create

00000000000215f4 <epoll_create>:
   215f4: 00000513  li a0,0
   215f8: fd5ff06f  j 215cc <epoll_create1>
```

也就是镜像中的 RISC-V musl `epoll_create(size)` 没有检查入参，而是直接清零 `a0` 并跳转到 `epoll_create1(0)`，导致 `size=0/-1` 都被内核当成合法新接口调用。

LoongArch64 镜像中的 musl libc 已有正确校验：

```text
00000000000218d0 <epoll_create>:
   218d0: bge $r0,$r4,12
   218d4: move $r4,$r0
   218d8: b 218a8 <epoll_create1>
   218dc: ...
   218e0: addi.w $r4,$r0,-22
   218e8: bl 2046c <__syscall_ret>
```

因此问题限定在 RISC-V 预赛 musl libc 包装函数。

## 根因

RISC-V 预赛镜像 `/musl/lib/libc.so` 的 `epoll_create(size)` 包装函数错误地省略了 `size <= 0` 校验，直接调用 `epoll_create1(0)`。内核无法从 syscall 20 的 `flags=0` 区分这是合法 `epoll_create1(0)` 还是 libc 包装后的非法旧接口调用。

## 修复

在 `os/src/fs/map_dynamic_link.rs` 的动态库只读兼容补丁路径中，为 `target_arch = "riscv64"` 新增 `patch_riscv64_musl_libc_epoll_create()`：

- 仅匹配 `/musl/lib/libc.so`。
- 将 `epoll_create` 入口 `0x215f4` 改为跳转到同一 RX LOAD 段内的零填充区域 `0x72c30`。
- 在 trampoline 中检查 `a0 <= 0`：
  - 非法时设置 `a0 = -EINVAL` 并跳 `__syscall_ret`。
  - 合法时设置 `a0 = 0` 并跳回 `epoll_create1`。
- 不修改底层 ext4 镜像，不改变内核 `sys_epoll_create1(flags)` 语义。

该方案避免把 libc 包装 bug 转嫁到 syscall 层，也不影响 LoongArch64 已正确的 musl libc。

## 涉及文件

- `os/src/fs/map_dynamic_link.rs`
- `Docs/决赛文档/开发日志.md`
- `Docs/决赛文档/problem/README.md`
- `Docs/决赛文档/problem/epoll-create02-riscv-musl-libc.md`
- `Docs/决赛文档/ai.log`
- `Docs/决赛文档/AI_INTERACTION.md`

## 验证

已执行：

```text
make TARGET_ARCH=riscv64
```

结果：构建通过，仅有既有 warning。

临时将 `initproc` 单测入口切换为 `epoll_create02` 后执行：

```text
timeout 120s make run > /tmp/epoll-create02-fix.log 2>&1
```

第一次在沙箱内运行时 QEMU 因 `/var/tmp` 只读无法创建临时文件失败；按权限流程在沙箱外重跑成功。关键输出：

```text
epoll_create.h:29: TINFO: Testing variant: syscall __NR_epoll_create
epoll_create.h:15: TCONF: syscall(-1) __NR_epoll_create not supported on your arch
epoll_create.h:32: TINFO: Testing variant: libc epoll_create()
epoll_create02.c:32: TPASS: epoll_create(0) : EINVAL (22)
epoll_create02.c:32: TPASS: epoll_create(-1) : EINVAL (22)

Summary:
passed   2
failed   0
broken   0
skipped  1
warnings 0
```

`FAIL LTP CASE epoll_create02 : 10` 仍会出现，这是因为旧 syscall 变体在当前架构 `TCONF/skipped` 后 LTP 进程返回 10；本仓库判读 LTP 结果以 `TPASS/TFAIL/TBROK/Summary` 为准。

验证后已恢复 `initproc` 原入口，并重新执行 `make TARGET_ARCH=riscv64` 保持构建产物与源码一致。

未执行 `TARGET_ARCH=loongarch64` QEMU 验证；LoongArch64 镜像 libc 反汇编显示已有 `size <= 0` 校验，本次代码只在 `#[cfg(target_arch = "riscv64")]` 下生效。
