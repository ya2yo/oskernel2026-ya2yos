# lmbench musl/glibc 连续运行的 ext4 `EEXIST` 析构自锁

## 背景

维护者提供的根目录 `log.ans` 包含三次 RISC-V 双 hart lmbench 运行：

1. `lmbench-musl` 完整结束，紧接着 `lmbench-glibc` 只打印到
   `Simple write`，随后出现 `QEMU: Terminated`。
2. 单独运行 `lmbench-glibc`，内核输出停在
   `task::add_initproc...QEMU: Terminated`。
3. 再次单独运行 `lmbench-glibc`，整组正常结束并 `shutdown!`。

目标是修复 musl 后接 glibc 的卡死，并把 musl lmbench 的每个实际测例单独验证，
不能仅因某次 QEMU 被终止就把最后一条输出当作根因。

## 原始日志取证

先将二进制日志中的 NUL 清理后按 QEMU 启动 banner 划分三段。

- 第一段的 `lmbench-musl` 有明确的
  `#### OS COMP TEST GROUP END lmbench-musl ####`，因此 musl 全组在该样本中没有卡住。
  glibc 已完成 `lat_syscall null/read/write`；按照镜像脚本，下一步不是 `stat`，而是
  `./busybox mkdir -p /var/tmp` 和 `./busybox touch /var/tmp/lmbench`。
- 第二段没有 `Entry Point: 0x10000`，说明终止发生在 `INITPROC` Lazy 的
  `open/read_all/from_elf` 区间；日志只有宿主发出的 `QEMU: Terminated`，没有 panic、
  锁栈或任务状态，不能据此把它和 lmbench 测例绑定。
- 第三段完整通过 glibc 的 signal、pipe、fork/exec、带宽和 context-switch 项，说明
  卡死依赖“本次启动已执行过 musl”这一前置状态，而不是某个 glibc 测例必现。

原始日志和三段样本均未出现 `panic`、`TFAIL`、`TBROK`、OOM 或 assertion。

## 测试入口与测例清单

测试脚本不在仓库源码中，而在默认 RISC-V ext4 镜像：

```text
2026_testsuits_img/pre_tests/sdcard-rv.img:/musl/lmbench_testcode.sh
2026_testsuits_img/pre_tests/sdcard-rv.img:/glibc/lmbench_testcode.sh
```

通过 `debugfs -R 'cat ...'` 读取脚本，确认两个版本均有 24 个 benchmark 调用：

```text
01  lat_syscall null                 13  lat_proc exec
02  lat_syscall read                 14  lat_proc shell
03  lat_syscall write                15  lmdd
04  lat_syscall stat                 16  lat_pagefault
05  lat_syscall fstat                17  lat_mmap
06  lat_syscall open                 18  lat_fs
07  lat_select file                  19  bw_pipe
08  lat_sig install                  20  bw_file_rd io_only
09  lat_sig catch                    21  bw_file_rd open2close
10  lat_sig prot                     22  bw_mmap_rd mmap_only
11  lat_pipe                         23  bw_mmap_rd open2close
12  lat_proc fork                    24  lat_ctx
```

仓库内旧的 `user/src/bin/lmbench/mod.rs` 将 `lat_sig prot`、`lat_pipe`、`bw_pipe`
注释掉，不能用于代表镜像实际脚本；诊断必须以镜像脚本为准。

## 调试过程

### 1. 排除 DEBUG 构建造成的伪超时

最初使用 `make log` 构建后运行单独 glibc，180 秒上限触发。该内核会为几乎每个
syscall 输出 DEBUG 日志，日志增长到约 18 MiB。过滤普通输出后可见测试已经到达最后
一项 `lat_ctx`，两个 pipe worker 仍在正常 read/write 循环；这不是卡死。

之后统一改用 release/warn 构建：

```bash
make build-arch TARGET_ARCH=riscv64
```

单独 glibc 在 release 下能完整 `END` 和 `shutdown!`，与第三段原始日志一致。

### 2. 复现真实组合问题

临时启用原有的两行入口，按真实顺序运行：

```rust
run_testsuit("musl\0", "lmbench_testcode.sh\0");
run_testsuit("glibc\0", "lmbench_testcode.sh\0");
```

release 下稳定得到：musl 全组 `END` 后，glibc 的 `null/read/write` 已输出，但后续
没有进展。QEMU 两个虚拟 hart 接近满载，表明是 guest 内自旋，而不是宿主暂停。

为标出 shell 的实际命令，临时将 `busybox sh` 改为 `busybox sh -x`。关键尾部为：

```text
+ ./lmbench_all lat_syscall -P 1 write
Simple write: ...
+ ./busybox mkdir -p /var/tmp
qemu-system-riscv64: terminating on signal 15 from ...
```

`touch` 和后续 `stat` 都没有出现。此时只知道 BusyBox 子进程没有从 `mkdir -p` 返回，
尚不能区分 fork/exec、mkdir syscall 或父 shell 的 wait。

### 3. 将 musl 的 24 项拆成独立样本

为避免把脚本顺序中的复杂 pipe/process 测试误判为根因，临时在 `initproc` 中对每一项：

1. 使用一个独立 BusyBox shell 执行该项；
2. 打印唯一 `CASE BEGIN/END` 标记；
3. 调用现有 `cleanup_testsuit_children()`；
4. 保留必要前置条件，例如 #4--#6 创建 `/var/tmp/lmbench`，#16/#17/#20--#23
   先生成 `/var/tmp/XXX`。

修复前，#1--#4 都能结束；#4 首次创建 `/var/tmp` 后，#5 的命令刚开始：

```text
#### LMBENCH MUSL CASE BEGIN 05-lat_syscall-fstat ####
["busybox\0", "sh\0", "-c\0", "./busybox mkdir -p /var/tmp; ..."]
```

就永久停止，尚未进入 `lat_syscall fstat`。这将问题收敛到重复 `mkdir -p`，并排除了
`lat_proc`、`bw_pipe`、`lat_ctx` 残留才触发的假设。

### 4. 调用链和析构顺序审计

BusyBox 的 `mkdir -p /var/tmp` 对已经存在的目录分量会接受 `EEXIST`，然后继续检查
该分量是否为目录。内核调用链为：

```text
sys_mkdirat
  -> open(O_CREATE | O_EXCL | O_DIRECTORY)
  -> create_file
  -> Ext4Inode::create
```

旧的 `Ext4Inode::create()` 为：

```rust
let _ext4 = EXT4_OP_LOCK.lock();
let nf = Ext4Inode::new(path, types.clone());

if file.check_inode_exist(path, types.clone()) {
    return Err(SysErrNo::EEXIST);
}
```

`EXT4_OP_LOCK` 是不可重入的 `spin::Mutex`。`nf` 是局部 `Ext4Inode`，而
`Ext4Inode::Drop` 为关闭底层 `Ext4File` 会再次获取 `EXT4_OP_LOCK`。Rust 的局部变量
按声明逆序析构，因此 `return Err(EEXIST)` 时实际顺序为：

```text
Ext4Inode::create 持有 EXT4_OP_LOCK
  -> check_inode_exist == true
  -> nf::Drop()
       -> EXT4_OP_LOCK.lock()  // 同一 hart 再次获取不可重入锁
  -> _ext4 尚未析构，永久自旋
```

`dir_mk()`、`file_open()`、`file_close()` 的其他错误返回也会触发同一局部对象析构顺序。
这解释了组合差异：musl 首次创建 `/var`、`/var/tmp` 正常；随后 glibc 或独立 #5 对同一
路径执行 `mkdir -p`，经 ext4 的 `EEXIST` 分支进入自锁。干净启动单独 glibc 没有这批
已创建目录，所以可以通过。

同时核对镜像 ELF：`/glibc/busybox` 与 `/glibc/lmbench_all` 均为静态链接，故本次不经过
动态链接器或 `PT_INTERP`；`execve` 与 `waitpid` 只是被卡住的 BusyBox child 不能退出后
呈现出的上层等待现象。

### 5. 未采纳的旁路结论

- `task::add_initproc...QEMU: Terminated` 的单独样本没有足够内核状态，且后续冷启动
  未复现；本次不把它归因于 lmbench，也不混入无证据的启动路径改动。
- 审计发现 `kill(-1)` 广播遇到并发消失任务时可能过早返回 `ESRCH` 的独立风险，但
  最小 #4 -> #5 复现发生在任何 pipe/process/context 测例之前，不能解释本问题，留作
  后续专项修复。
- CFS `Running` entry 契约缺口同样没有在卡死日志中出现 `fetch_task` 丢弃警告；不以
  未证实的调度改动掩盖 ext4 的确定自锁。

## 根因

`Ext4Inode::create()` 让会在 `Drop` 中获取 `EXT4_OP_LOCK` 的临时 inode，在已持有该锁
之后才构造。任何创建失败路径，尤其是重复 mkdir 产生的 `EEXIST`，都会在释放 guard
之前析构临时 inode，形成同 hart 不可恢复的自旋锁自锁。

## 修复

仅调整局部变量的构造顺序：

```rust
let types = as_ext4_de_type(ty);
let nf = Ext4Inode::new(path, types.clone());
let _ext4 = EXT4_OP_LOCK.lock();
let file = &mut self.inner.get_unchecked_mut().f;
```

`Ext4File::new()` 只初始化 Rust 结构和路径字符串，不访问 lwext4，因此可安全放在锁外。
存在性检查与真实创建仍都在同一个 `EXT4_OP_LOCK` 临界区内，不引入并发创建窗口。错误返回
时的析构顺序变为先释放 `_ext4`，再调用 `nf::Drop()`，消除了重入锁。

## 涉及文件

- `os/src/fs/ext4_lw/inode.rs`
- `Docs/决赛文档/problem/lmbench-ext4-eexist-drop-self-deadlock.md`
- `Docs/决赛文档/problem/README.md`
- `Docs/决赛文档/开发日志.md`
- `Docs/决赛文档/ai.log`
- `Docs/决赛文档/AI_INTERACTION.md`

临时矩阵和 shell `-x` 均已从 `user/src/bin/initproc.rs` 撤回；lmbench 的常规入口选择
保留工作区当前的维护者配置，未作为本次内核修复的一部分改写。

## 验证

```bash
make build-arch TARGET_ARCH=riscv64
timeout 720s make run > /tmp/lmbench-musl-case-matrix-after-ext4-create-fix.log 2>&1
timeout 420s make run > /tmp/lmbench-musl-glibc-after-ext4-create-fix.log 2>&1
make build-arch TARGET_ARCH=loongarch64
git diff --check
```

- RISC-V release 构建通过。
- 单项矩阵中 24 个 musl 测例均输出 `CASE END ... status=0`；随后完整 glibc 输出
  `#### OS COMP TEST GROUP END lmbench-glibc ####` 和 `shutdown!`。
- 真实 musl 脚本后接真实 glibc 脚本同样都输出 `GROUP END` 与 `shutdown!`；已越过原先
  glibc `Simple write` 后的 `mkdir -p` 卡点。
- LoongArch64 release 构建通过；本轮未运行 LoongArch64 lmbench 行为回归。
- 测试镜像的 `/tmp/hello` wrapper 仍引用不存在的
  `/code/lmbench_src/bin/build/lmbench_all`，因此 `lat_proc shell` 仍打印 13 条
  `not found`。脚本会继续结束，这属于镜像测试包装路径问题，未在本次修改内修复，不能将
  该项性能值视为有效 benchmark 数据。
