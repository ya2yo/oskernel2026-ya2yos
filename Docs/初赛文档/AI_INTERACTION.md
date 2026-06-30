# AI 使用记录

## 声明

本项目在开发过程中使用了以下 AI 工具及大模型：

| 类别 | 名称 |
| ------ | ------ |
| AI 编程工具 | Cursor, Claude Code |
| 大语言模型 | GPT-5.5 (ChatGPT), Gemini 3 Flash Preview, DeepSeek-v4, Claude Opus 4.7 |

所有 AI 工具的使用均限于**辅助开发**（代码分析、Bug 定位、代码生成建议、文档完善），最终代码决策由人工审核后采纳。按照大赛要求，本文档及开发日志、项目设计文档中均设有 AI 使用专门章节，git commit 记录中标注了 AI 使用情况。由于部分早期 commit 的 AI 标注不够完整，本文档作为最全面的补充说明。

---

## AI 使用场景分类

### 1. 代码理解与注释

- **StarryOS 网络模块注释**（主要工作）：使用 ChatGPT、Gemini 等模型逐文件阅读和分析 StarryOS 的网络模块代码，为以下核心文件添加了详细的中文注释：
  - `os/src/net/mod.rs`（120行）：网络子系统初始化流程、全局单例（LISTEN_TABLE、SOCKET_SET、SERVICE）、poll_interfaces 轮询机制
  - `os/src/net/socket.rs`（300+行）：套接字抽象层，包括 `SocketOps` trait 的所有方法语义、`SendFlags`/`RecvFlags` 每个标志位的 POSIX 语义、`SocketAddrEx` 枚举设计
  - `os/src/net/tcp.rs`（560+行）：TCP 状态机（`Idle → Connecting → Connected → Closed`）、三次握手流程、send/recv 的 PEEK 支持、shutdown 的读写半关闭逻辑
  - `os/src/net/udp.rs`（360行）：UDP 无连接语义、sendto/recvfrom 的地址处理、`ExpectedRemote` 枚举区分已连接/未连接模式
  - `os/src/net/listen_table.rs`（195行）：端口监听表的数据结构（`Arc<Mutex<Option<Box<...>>>>` 设计）、SYN 队列管理、`incoming_tcp_packet` 的协议栈底层回调
  - `os/src/net/general.rs`（160行）：通用套接字选项（非阻塞模式、超时、地址重用）及 `send_poller`/`recv_poller` 的轮询-阻塞桥接模式
  - `os/src/task/future/mod.rs`（167行）：`Future` 执行器、`MyWaker` 的弱引用设计、`block_on` 的执行流程、`interruptible` 的可中断包装
- 为 `pci_config` 相关函数、`trap.S` 等底层汇编代码添加注释

### 2. 代码生成

- **基于测例驱动的代码补充**：通过分析 initproc.rs 中注册的测试套件（`musl`/`glibc` × `basic`/`busybox`/`lua`/`iozone`/`libcbench`/`libctest`/`lmbench`/`ltp`/`cyclictest`），确定每个测例依赖的系统调用，使用 AI 辅助逐个实现缺失的 syscall
- **Socket 系统调用测试**：生成 `test_socket()` 函数（`user/src/bin/initproc.rs:470`），涵盖 TCP/UDP 套接字的创建、绑定、监听、发送、接收全流程
- 辅助实现 `recvfrom`、`recvmsg`、`sendmsg` 等系统调用及其与 `msghdr` 结构体的交互
- 辅助实现 `setsockopt`/`getsockopt`、Unix 域套接字（`SOCK_STREAM`/`SOCK_DGRAM`）等功能
- 辅助重构 `initproc.rs` 中测试用例的组织结构（`run_testsuit` 统一入口）
- 辅助完成 `copy_from_user` 和 `copy_to_user` 的安全读写模式

### 3. Bug 分析与定位

- 分析日志输出定位死循环、死锁问题
- 分析内核 panic 的根因（页表、COW、缺页处理等）
- 分析龙芯架构特有的架构问题（PageModifyFault、指令异常等）

### 4. 架构适配与调试

- 龙芯架构 COW 机制分析与修复
- LoongArch 页表标志位处理
- 信号处理机制调试
- Pipe 阻塞机制重新设计

### 5. 文档完善

- 开发日志的整理与补充
- 问题解决记录的撰写

---

## 详细时间线

### 第一阶段：网络模块迁移与基础开发（5月初 - 5.15）

#### 迁移 StarryOS 网络模块（5月初）

- **工具/模型**：ChatGPT, Gemini, DeepSeek
- **场景**：代码理解与注释
- **描述**：StarryOS 的网络模块基于 smoltcp 协议栈和异步 Future 模型，涉及 TCP 状态机、UDP 无连接传输、端口监听表、全局套接字集合等组件。代码原本几乎没有注释，且使用了 `Arc<Mutex<Option<Box<...>>>>` 等复杂的 Rust 嵌套类型。使用多个大模型逐文件分析代码逻辑，为全部 15+ 个文件添加了详细的中文注释，涵盖每个结构体的职责、每个方法的协议语义、每个标志位的 POSIX 含义。
- **具体文件**：`os/src/net/{mod, socket, tcp, udp, general, listen_table, options, service, device, state, router, wrapper, consts, unix}.rs`，以及 `os/src/task/future/mod.rs`
- **关联 commit**：`6bafdb6`（merge starry 分支，commit message 标注了 ChatGPT, Gemini, DeepSeek）

#### 生成 Socket 测试用例（5月初）

- **工具/模型**：ChatGPT
- **场景**：代码生成
- **描述**：利用 AI 生成 socket 系统调用（`sys_socket`、`sys_bind`、`sys_listen`、`sys_accept` 等）的基础测试用例。
- **关联 commit**：`ef5703d`, `7b447ed`

#### 测例驱动的系统调用补充（5月初 - 5.25，持续进行）

- **工具/模型**：Cursor, ChatGPT, Gemini
- **场景**：代码生成
- **描述**：整个开发的推进方式是"以测促补"——根据 initproc.rs 中注册的测试套件运行结果，确定缺失或错误的系统调用，使用 AI 辅助实现。initproc.rs 中维护了两组测试矩阵（musl/glibc × 10+ 个测试脚本），每个测试脚本对应一组系统调用依赖。
- **典型流程**：
  1. 运行某个测试脚本（如 `iozone_testcode.sh`）
  2. 内核 panic 或返回错误码
  3. 将错误日志提供给 AI（Cursor），询问"这个测例需要哪些系统调用？当前缺少什么？"
  4. AI 分析后列出缺失的 syscall 及其推荐实现方式
  5. 人工审查后采纳或调整
  6. 重新运行测试验证
- **通过此方式实现的主要系统调用**：`sys_statx`、`sys_sendmsg`、`sys_recvmsg`、`sys_rt_sigsuspend`、`sys_futex` 的多种操作、`sys_clone3` 等

#### 重构 initproc.rs（5月初）

- **工具/模型**：Cursor + GPT
- **场景**：代码生成 / 重构
- **描述**：使用 AI 调整 `initproc.rs` 的代码结构，使其更清晰。
- **关联 commit**：`8a64e7c`

---

### 第二阶段：网络系统调用完善（5.15 - 5.18）

#### 增强 recv/send 系统调用（5.15）

- **工具/模型**：Cursor
- **场景**：代码生成
- **描述**：使用 Cursor 完成 `recvfrom`、`recvmsg`、`sendmsg` 的扩展实现，包括逻辑重构和对 `msghdr` 内部 `msg_control` 的处理。
- **关联 commit**：`a7bd6df`, `7210d11`

#### 修复网络系统调用 Bug（5.17）

- **工具/模型**：Cursor
- **场景**：Bug 定位与修复
- **描述**：使用 Cursor 分析并修复了多个网络相关 bug：
  - UDP `sendto`/`recvfrom`：修复 UDP 发送时只读了用户 buffer 但未拷贝进 smoltcp 发送 buffer 的问题
  - `SOCK_NONBLOCK`：`sys_socket`/`sys_socketpair` 将 `O_NONBLOCK` 写入 fd flags
  - `F_SETFL`：不仅修改 fd table，还同步调用底层 `set_nonblocking()`
  - TCP `listen`/`connect`：恢复 TCP SYN 进入协议栈前的监听表 snoop，修复 `TcpSocket::connect()` 中 `connect(...).map_err(...)` 结果被忽略的问题
- **关联 commit**：`d4546bd`

#### 实现 Unix 套接字（5.23）

- **工具/模型**：Cursor + GPT
- **场景**：代码生成
- **描述**：辅助实现 Unix 域套接字的基本功能。
- **关联 commit**：`2a33380`

---

### 第三阶段：调试与 Bug 修复（5.19 - 5.23）

#### 线程5僵尸线程死循环 — block_on 异常引用计数（5.19）

- **工具/模型**：Cursor
- **场景**：Bug 分析与定位
- **描述**：内核在运行 LTP 测试前几个测例时出现死循环。使用 Cursor 对内核输出的 debug 日志进行分析。日志关键片段：

  ```text
  [block_on] strong count: 3
  [block_on] Pending strong_count = 4
  ```

  `block_on` 函数（`os/src/task/future/mod.rs:81`）在进入 Pending 状态前后各打印一次 `Arc::strong_count`。第一次强引用计数为 3，第二次却变成了 4（代码注释："这里怎么比上面多一个"）。Cursor 分析指出：在 `block_on` 循环中，`MyWaker` 持有 `WeakTaskRef`（弱引用），但 `poll()` 调用返回 `Pending` 后，Future 内部可能通过 Waker 保存了对 Task 的额外强引用。当异步网络操作（如 `accept`、`recvfrom`）返回 Pending 时，其关联的 Future 未运行完成，但内部对 Task 的强引用不被释放，导致任务引用计数异常，永远无法进入就绪态，表现为死循环。

- **根因**：引入 StarryOS 网络模块时未能充分理解异步编程思想——`MyWaker` 虽然设计为 `WeakTaskRef` 防止循环引用，但 Future 的 `poll()` 闭包在挂起期间通过 `current_task()` 额外持有了 `Arc<Task>`，导致强引用计数永远不会降到 0，任务无法被正确回收。

- **修复思路**：在 `block_on` 的 Pending 分支中，确保在调用 `block_current_and_run_next()` 休眠当前任务之前，显式 drop 掉对 Task 的强引用，使调度器能正确处理任务生命周期。

- **关联 commit**：`52ac423`, `c32d05c`

#### 修复 translate.rs 安全性问题（5.23）

- **工具/模型**：ChatGPT 5.5
- **场景**：Bug 修复 / 代码重构
- **描述**：使用 ChatGPT 5.5 分析和修改 `translate.rs` 中的 `safe_translated` 函数。该函数名为 "safe" 但实际并不安全——会直接对未映射地址调用 `.unwrap()` 导致 panic，使 `get_cwd` 无法正确返回错误，违背了 Linux 语义。修改为使用 `copy_to_user` 模式处理。
- **关联 commit**：`30d50b4`

#### cgroup_fj 死循环分析（5.22 - 5.23）

- **工具/模型**：GPT-5.5
- **场景**：Bug 分析 / 测试策略
- **描述**：使用 GPT-5.5 分析 cgroup-fj 最后死循环的原因。AI 分析发现 `cgroup_fj_function.sh` 中某些子测试失败后导致 `cgroup_fj_proc` 收不到退出信号而卡死。按照 AI 建议选择单独测试这组测例，显式调用 `./cgroup_fj_function.sh cpuset` 避开失败的子测试。
- **关联 commit**：`53a15f4`, `a24b7f7`

#### 信号处理 Bug 修复（5.22）

- **工具/模型**：GPT-5.5
- **场景**：Bug 分析
- **描述**：分析 `sys_rt_sigsuspend` 中 pending 信号的判断逻辑——原来是直接判断 pending 是否为空，正确的逻辑应该是和 mask 做比较。
- **关联 commit**：`4f739a2`

---

### 第四阶段：龙芯架构适配（5.23 - 5.25）

#### 龙芯架构工具链配置（5.23）

- **工具/模型**：GPT-5.5 + Cursor
- **场景**：架构适配
- **描述**：使用大模型辅助配置龙芯架构的交叉编译工具链、GDB 调试环境、编译脚本参数。解决了链接器配置、PCI BAR 参数等问题。
- **关联 commit**：`a946f6d`, `a144ab0`, `ebcb554`, `cc55f7f`, `a283a24`

#### 龙芯网卡初始化与放弃（5.24）

- **工具/模型**：Cursor
- **场景**：Bug 分析 / 决策
- **描述**：分析龙芯架构下网卡驱动初始化死循环的问题。经过 GDB 反汇编分析（发现卡在 `spin` 区域），决定暂时跳过网卡设备初始化，优先保证其他测试通过。
- **关联 commit**：`a978b5c`, `0e03c75`

#### 龙芯 busybox 测试失败（5.24）

- **工具/模型**：Cursor
- **场景**：Bug 定位与修复
- **描述**：使用 Cursor 分析 `log.ans` 末尾的 `LoadPageFault`。根因是 LoongArch 的 COW 页表标志处理不完整：
  - fork 后私有 mmap/brk 页进入 COW 时只清了 `WRITEABLE`，没有清 `DIRTY`
  - 解除 COW 后也没有补回 `DIRTY` 并刷新 TLB
  - 结果父子进程的私有堆/mmap 页写入隔离不可靠，busybox shell 读到被污染的 malloc 状态后触发 LoadPageFault
- **修改文件**：
  - `os/src/trap/mod.rs`：LoongArch `PageModifyFault` 先尝试走 COW handler
  - `os/src/arch/loongarch64/qemu/page_table.rs`：进入 COW 时清 `WRITEABLE | DIRTY`，解除 COW 时恢复并刷新 TLB
- **关联 commit**：`2ae92cc`

#### 龙芯 libcbench_testcode 死循环（5.25）

- **工具/模型**：Cursor
- **场景**：Bug 分析与修复
- **描述**：使用 Cursor 分析 log 输出，发现龙芯 `PageModifyFault` 处理读错了 fault address——原来使用 `tlbrbadv`，但普通 PageModifyFault 应该读 `badv` 寄存器。结果 dirty bit 被设置到错误的页上，用户态同一条 `stptr.d` 反复触发页修改异常，表现为死循环。
- **修改内容**：
  - `os/src/arch/loongarch64/qemu/trap_interface.rs`：`tlb_page_modify_handler()` 改用 `badv::read().vaddr()`
  - `os/src/trap/mod.rs`：启用非法指令处理；timer 中断分支内立即重装下一次 timer
  - `os/src/trap/trap_types.rs`：新增 `IllegalInstruction` 类型
  - `os/src/timer.rs`：恢复 `TICKS_PER_SEC` 频率
- **关联 commit**：`3c60ee8`

---

### 第五阶段：RISC-V 与龙芯最终冲刺（5.26 - 5.27）

#### RISC-V 通过 lmbench（5.26）

- **工具/模型**：Cursor
- **场景**：Bug 定位与修复
- **描述**：使用 Cursor 分析 lmbench 测试卡住问题。AI 分析过程：
  1. 初始现象：`Protection fault` 异常
  2. Cursor 指出这是 lmbench 的保护异常测试——故意向只读 mmap 页写入，期望内核产生 SIGSEGV/SIGBUS 由用户 signal handler 捕获
  3. 原实现直接退出进程，改为判断进程是否注册 `sig_handler` 来发送信号
  4. 改完仍死循环，GDB 发现 `sepc` 没变，继续分析
  5. Cursor 分析发现 `Protection fault` 本身已完成，真正卡住的是 `lat_pipe` 测试——原来的 pipe 实现中空读/满写时只做 yield 而非阻塞等待
- **修改内容**：
  - `os/src/fs/files/pipe.rs`：为 pipe ring buffer 增加 `read_waiters` / `write_waiters` 等待队列，实现真正的阻塞读/写
  - `os/src/task/mod.rs`：增加 `schedule_blocked_current()`
  - `os/src/signal/mod.rs`：给阻塞态任务发信号时将其改回 Ready 并放回 ready queue
  - `os/src/arch/riscv64/qemu/page_table.rs`：同步龙芯的 COW 修复（清 WRITEABLE|DIRTY，恢复并刷新 TLB）
- **关联 commit**：`592ecf2`

#### 龙芯 + RISC-V glibc-iozone 修复（5.27）

- **工具/模型**：Claude Code + DeepSeek-v4
- **场景**：Bug 定位与修复
- **描述**：使用 Claude Code 搭配 DeepSeek-v4 模型解决两个架构的 iozone-glibc 测试问题。
- **龙芯架构修复过程**：
  1. 修复 glibc ld-linux 路径映射错误：`os/src/fs/map_dynamic_link.rs` 中将 `/lib64/ld-linux-loongarch-lp64d.so.1` 从错误映射到 musl 的 libc.so 改为正确映射到 glibc 的 ld-linux
  2. 补充 `libc.so.6` 路径映射：`/usr/lib64/libc.so.6` → `/glibc/lib/libc.so.6`
  3. 修复 `execve` 后 `clear_child_tid` 指向旧地址空间导致 panic：在 `exec()` 中切换地址空间之前，先在旧空间中完成 `clear_child_tid` 的写零和 futex_wake
- **RISC-V 架构修复过程**：
  1. `sys_statx` 中 `translated_str` 替换为 `copy_from_user` 模式，解决缺页导致的 panic
- **关联 commit**：`6aa8b81`, `73aff7c`

---

### 第六阶段：文档完善（5.27）

- **工具/模型**：Claude Code + DeepSeek-v4
- **场景**：文档完善
- **描述**：使用 Claude Code 完善开发日志（`开发日志.md`）、问题复盘（`problem/`）和本文档。
- **关联 commit**：`d2c20ba`, `31e3369`

---

### 第七阶段：wait4 / 托孤与网络组播（5.28 - 5.29）

#### wait4 阻塞未处理信号（5.28）

- **工具/模型**：Claude
- **场景**：Bug 分析与定位
- **描述**：libctest、lmbench 卡死；对用户态二进制反汇编定位到 `wait4` 循环。AI 指出 wait 阻塞前未检查 pending 信号，与 Linux 可中断阻塞语义不符。人工确认后在 `wait.rs` 增加信号分支并用 `interruptible` 包装。过程详见 `ai.log` 2026-05-28 条目。
- **关联 commit**：`3fc325b`, `6762618`

#### 进程退出托孤 exit_and_reparent（5.28）

- **工具/模型**：人工为主
- **场景**：Bug 修复
- **描述**：父进程先于子进程退出时，子进程应挂到 initproc。实现 `Process::exit_and_reparent` 并在进程全线程退出时调用。
- **关联 commit**：`6762618`

#### accept02 组播与 setsockopt（5.29）

- **工具/模型**：Cursor (Composer)
- **场景**：Bug 分析、代码生成、日志分析
- **描述**：多轮 `log.ans` 分析：IGMP 源地址（IP 注册顺序）、组播以太网发送、router 断言、virtio token；对照 LTP accept02 与 `tcp.rs` 确认 socket 层语义正确，定位 `sys_setsockopt` 未传播 `EADDRNOTAVAIL`。AI 辅助实现组播 MAC 映射与 JoinGroup/LeaveGroup。过程详见 `ai.log` 2026-05-29 条目。
- **关联 commit**：`e5d1b07`, `4a9b73c`, `5f90be8`

#### 项目 Agent 技能与文档规范（5.29）

- **工具/模型**：Cursor (Composer)
- **场景**：文档完善
- **描述**：编写 `.claude/skills/` 下 build-and-test、ltp-test-triage、network-debug 等技能；定义开发日志（简约）/ problem / ai.log / AI_INTERACTION 四份文档分工。
- **关联文件**：`.claude/skills/doc-writing/SKILL.md`

#### LTP access04 mount / loop 设备（5.30）

- **工具/模型**：Cursor (Composer)
- **场景**：日志分析、Bug 修复、代码生成
- **描述**：多轮 `log.ans` 排查 LTP access04：mount 缺页 panic → `copy_from_user`；LoongArch TBROK → 实现 loop 块设备与 `/dev/loop-control`；tmpfs `special=NULL` EFAULT → 空指针转空串；LA `handle_mprotect` 懒分配页修复。用户验证 LoongArch musl 通过后补文档。过程详见 `ai.log` 2026-05-30 条目与 [problem/access04-ltp-musl.md](./problem/access04-ltp-musl.md)。
- **关联 commit**：`7741ee6`

#### LTP access02 execve shebang（5.30）

- **工具/模型**：Cursor (Composer)
- **场景**：日志分析、Bug 修复
- **描述**：提供 LoongArch `log.ans`，`access02` 在 X_OK 执行验证阶段 4× TFAIL；对照 LTP 源码确认 `file_x` 为 `#!/bin/sh` 脚本；定位 `sys_execve` 对非 ELF 直接 `ENOEXEC`。AI 实现 shebang 解析与解释器 argv 重建，用户验证通过后补文档。详见 `ai.log` 2026-05-30 access02 条目与 [problem/access02-ltp-execve.md](./problem/access02-ltp-execve.md)。
- **关联 commit**：`6a77ff0`, `dedc633`

#### setresgid(149) 系统调用（5.31）

- **工具/模型**：Cursor (Composer)
- **场景**：代码生成、测例验证
- **描述**：按 syscall-implementation skill 实现 `setresgid`/`getresgid`：TCB 维护 GID 三元组、Linux 级联语义与非特权 EPERM 检查；修正 `GetResgid` 编号 148→150。RISC-V `setresgid01` 5× TPASS。详见 `ai.log` 2026-05-31 条目与 [problem/setresgid-syscall.md](./problem/setresgid-syscall.md)。
- **关联 commit**：`a7b9a9f`

#### clone03 MAP_SHARED fork 帧共享修复（5.31）

- **工具/模型**：Cursor (Composer)
- **场景**：Bug 分析与定位
- **描述**：提供 `log.ans`，AI 分析 clone03 失败日志，定位 `from_existed_user` 中 MAP_SHARED 懒分配区域 fork 后父子各自独立分配物理帧，破坏共享语义。同时定位 `recycle_data_pages` 中 `MAP_ANONYMOUS` 区域 `unwrap() None` panic。实现预 fault pass（MAP_ANONYMOUS 零页 / 文件支撑读文件），并增加 `is_some()` 检查。详见 `ai.log` 与 [problem/clone-mmap-shared-fork.md](./problem/clone-mmap-shared-fork.md)。
- **关联 commit**：`5a308d4`, `9e67097`

#### clone05 CLONE_VFORK 挂起机制（5.31）

- **工具/模型**：Cursor (Composer)
- **场景**：Bug 分析与定位
- **描述**：提供 `log.ans`，AI 分析 clone05 测试失败原因：内核完全未实现 CLONE_VFORK 挂起。第一轮修复后持续失败，对比两轮日志定位三处调度路径（suspend_current_and_run_next 无条件 Ready、run_tasks 无差别入队、空队列 keep-running）绕过 VforkBlocked。逐一修复后测试通过。详见 `ai.log` 2026-05-31 条目与 [problem/clone05-vfork.md](./problem/clone05-vfork.md)。
- **关联 commit**：`9110e7e`, `bb0d7d0`

#### LTP creat04 open 权限检查修复（6.2）

- **工具/模型**：Claude Code (Claude Opus 4.7)
- **场景**：Bug 分析与定位、代码生成、日志调试
- **描述**：用户提供 `log.ans` 要求分析"哪个应该失败的 syscall 返回成功"。AI 追踪 syscall 调用序列，定位 `create_file` 完全无权限检查、`sys_fchownat` 是 stub、`open()` 已有文件无写检查三个根因。AI 生成 owner/group/other 三级权限检查代码。第一版使用 `user_id`（real uid）判定 root，用户重跑仍 TFAIL；AI 添加 debug 日志后发现 `setresuid(-1,u,-1)` 只改 effective uid 不改 real uid，修正为 `effective_uid`。详见 `ai.log` 2026-06-02 条目与 [problem/creat04-open-permission.md](./problem/creat04-open-permission.md)。
- **关联 commit**：`3c2803e`, `7db430b`

#### lwext4 重构与 sys_linkat 文档（6.3）

- **工具/模型**：Claude Code (Claude Opus 4.7)
- **场景**：代码理解与文档完善
- **描述**：用户询问 `sys_linkat` 实际工作是否在 lwext4_rust 中完成。AI 查看 commit `857ede1` 的完整 diff（6 文件、98 增/69 删），梳理四层调用链（syscall → VFS → ext4 适配 → lwext4 FFI），按 doc-writing skill 模板生成详细文档。详见 `ai.log` 2026-06-03 条目与 [problem/linkat-hardlink-refactor.md](./problem/linkat-hardlink-refactor.md)。
- **关联 commit**：`857ede1`

#### Task 凭证字段注释（6.3）

- **工具/模型**：Claude Code (Claude Opus 4.7)
- **场景**：代码理解与注释
- **描述**：用户询问 Task 中 6 个 uid/gid 字段的用途，AI 添加注释块说明 POSIX 凭证三元组的区别（real/effective/saved）及各自在文件权限检查中的用途。详见 `ai.log` 2026-06-03 条目。
- **关联 commit**：`0d4205b`

#### 批量 syscall 实现：mincore/mlock/flock/mknodat/xattr/inotify 等（6.4-6.5）

- **工具/模型**：DeepSeek
- **场景**：代码生成
- **描述**：用户使用 deepseek 批量生成多个系统调用的基础代码框架，包括 mincore、mlock 系列、flock、mknodat、xattr、setreuid/setregid、inotify、sys_fsconfig、priority/rlimit 等。人工审核后集成到内核。详见 `ai.log` 2026-06-04 和 2026-06-05 条目。
- **关联 commit**：`b80e7e8`, `303b7a6`, `8b0b4c9`, `737f540`, `9d380f5`, `801e530`, `1442d36`, `663df8a`, `a13ec6c`

#### getpeername01 bug 修复（6.5）

- **工具/模型**：DeepSeek
- **场景**：Bug 分析与定位
- **描述**：提供 getpeername01 测试失败信息，AI 分析定位 addrlen 参数传递问题。修复后 4 个 LTP 测试用例通过。详见 `ai.log` 2026-06-05 条目。
- **关联 commit**：`f80ceed`

#### 系统调用冲刺：setgid/personality/msg/mq/clone3/unshare 等（6.6）

- **工具/模型**：DeepSeek
- **场景**：代码生成
- **描述**：用户使用 deepseek 批量生成 20+ 系统调用的基础代码，包括 setgid(144)、personality、msg 系列、mq 系列、clone3、unshare、memopolicy、umask、fdatasync、sync_file_range 等。ext4 符号链接读取也通过 deepseek 辅助完成。人工审核修改后合入。详见 `ai.log` 2026-06-06 条目。
- **关联 commit**：`8a63423`, `65b0571`, `5050b18`, `029bf8e`, `2a80f7a`, `7e1d309`, `ae9642c`, `9ebdfcb`, `a36c048`, `2e1ef32`, `ead5c9e`

#### futex 退出处理机制 bug 修复（6.6）

- **工具/模型**：DeepSeek
- **场景**：Bug 分析与定位
- **描述**：提供 log.ans，deepseek 辅助检查日志定位 futex 退出时处理机制的 bug，包括 clear_child_tid 已 unmap 导致 translate_va unwrap panic 和兄弟线程 futex 未唤醒等。详见 `ai.log` 2026-06-06 条目。
- **关联 commit**：`66d6983`、`7905fb7`

#### sigtimedwait 实现与 pthread_cancel_points 分析（6.6）

- **工具/模型**：Claude Code (Claude Opus 4.7)
- **场景**：Bug 分析与定位、代码生成
- **描述**：用户提供 pthread_cancel_points.c 测试代码和 log.ans。AI 逐场景追踪 7 个测试场景的 TID 调用序列、futex 事件和 Tgkill 信号传递，定位到：1) cancel 信号在 PTHREAD_CANCEL_DISABLE 时被过早消费；2) sys_rt_sigtimedwait 为伪实现（始终返回假信号 0）破坏 glibc 取消机制。AI 实现了完整的 sigtimedwait（解析 sigset+timeout → 定时器超时 → 循环检查 pending 信号 → 消耗信号/返回 signo）。用户确认修复后 crash 消失，仅剩 glibc 调度竞态导致的 "non-blocking pthread_join" 1 个失败。详见 `ai.log` 2026-06-06 条目。
- **关联 commit**：`cdb1c9e`

---

## AI 成果总结

### 按场景统计

| 使用场景 | 次数（约） | 涉及 commit 数 |
| ---------- | ----------- | --------------- |
| 代码理解与注释 | 6 | 5+ |
| 代码生成 | 15 | 40+ |
| Bug 分析与定位 | 15 | 25+ |
| 架构适配与调试 | 5 | 15+ |
| 文档完善 | 6 | 5+ |

### 关键成果

1. **网络模块成功移植**：在 AI 辅助下理解了 StarryOS 的异步网络设计，成功将网络模块迁移至本项目，实现了 socket、bind、listen、accept、sendto、recvfrom 等核心网络系统调用。

2. **多个关键 Bug 修复**：
   - 线程5僵尸线程死循环（Future 强引用问题）
   - cgroup_fj 死循环（信号未发送问题）
   - 龙芯 COW 页表标志不完整（WRITEABLE|DIRTY 标志位）
   - 龙芯 PageModifyFault 读错 fault address（tlbrbadv → badv）
   - Pipe 阻塞机制重新设计（yield → 真正的阻塞等待队列）
   - glibc 动态链接路径映射错误

3. **双架构通过核心测试**：龙芯和 RISC-V 均通过了 busybox、lmbench、iozone-glibc 等核心测试。

### AI 工具使用方式

本次开发中 AI 工具的主要使用模式为：

1. **分析日志** → 将内核输出日志提供给 AI 分析，定位异常点
2. **GDB 反汇编分析** → 将 GDB 输出提供给 AI，分析汇编级别的卡死原因
3. **代码审查** → 将相关代码片段提供给 AI，识别逻辑错误或遗漏
4. **代码生成** → 描述需求，由 AI 生成初始实现，人工审核后修改采纳
5. **架构知识查询** → 向 AI 查询龙芯架构的页表、异常处理等底层细节

所有 AI 生成的代码或建议均经过人工审查和测试验证后才合入代码库。

#### pthread_robust_detach 地址转换重构与 exit_signal 连锁修复（6.6）

- **工具/模型**：Claude (Chat Mode + Code)
- **场景**：Bug 分析与定位 + 代码重构
- **描述**：
  1. 用户要求将 `mm/translate.rs` 中 unsafe 地址转换函数（translated_byte_buffer 等）全部替换为 `copy_from_user`/`copy_to_user` 安全接口。AI 设计了 `copy_from_user_val<T>` / `copy_to_user_val<T>` 泛型封装，并逐文件替换 12 个文件中的 30+ 处调用。
  2. 用户提供 `log.ans` 报 panic，AI 分析定位三个连锁 bug：VA 非法 panic（VirtAddr::from → try_from）、sigtimedwait 残留 interrupted 标志（加 clear_interrupt）、CLONE_THREAD 覆盖 exit_signal（加 !CLONE_THREAD 守卫）。
  3. 用户要求移除 `task.get_fd_table()` 统一锁路径，AI 修改 20+ 处调用为 `proc_inner.fd_table`。
  4. AI 通过添加调试日志 `sys_waitpid: my children (pid, exit_sig, all_exited): [(3, -1, true)]` 精确定位 exit_signal = -1 的根因。
  5. 人工确认所有修改逻辑正确，测试通过。
  过程详见根目录 `ai.log` 2026-06-06 条目。
- **关联 commit**：`87f6933`、`b694ea6`、`27e3e6b`、`3afbf09`

#### 新挂载 API 基础实现与 LTP 黑名单整理（6.10）

- **工具/模型**：GPT-5.5
- **场景**：代码生成、系统调用兼容实现、测试列表维护
- **描述**：根据 2026-06-10 git 日志，用户使用 GPT-5.5 生成 Linux 新挂载 API 的主体实现，并人工集成提交 `c4d2e34`。该实现新增 `FsContextFd`、`DetachedMountFd`、`FsContext`、`FsConfigOption` 等结构，接入 `fsopen/fsconfig/fsmount/fspick/open_tree/move_mount/mount_setattr`，补齐 fd 类型区分、flags 校验、`FSCONFIG_*` 命令参数校验和简化状态记录。后续提交 `bdecf26` 追加 30 项 LTP 黑名单，覆盖重型压力、网络接口/Geneve、IMA、memcg、ftest 等当前内核尚不支持或不适合全量跑测的用例。详见 [problem/fsconfig-syscall.md](./problem/fsconfig-syscall.md)。
- **关联 commit**：`c4d2e34`、`bdecf26`

#### getrusage03 ru_maxrss / RUSAGE_CHILDREN 修复与文档补充（6.11）

- **工具/模型**：Codex (GPT-5)
- **场景**：Bug 分析与定位、代码修复、文档完善
- **描述**：用户提供 `log.ans`，要求根据日志修复 glibc LTP `getrusage03`，并根据此前 `fsconfig` 修复过程补文档。AI 分析日志定位 `/proc/self/status` 缺失、`waitpid` 未累计 `RUSAGE_CHILDREN`、进程退出过早删除 `/proc/<pid>`、`ru_maxrss` 始终为 0、256MiB 物理内存不足以支撑 300MiB 触页等问题。第一轮修复后仍有 `Expected 1 conversions got 0 FILE '/proc/self/status'`，AI 通过 `debugfs` 从测试镜像导出 `getrusage03` 并用 `strings` 确认 LTP 实际扫描 `VmSwap: %lu`，随后补 `VmSwap: 0 kB`。最终 RISC-V 单跑 `getrusage03` 四项 TPASS。详见 `ai.log` 2026-06-11 条目、[problem/getrusage03-rusage-proc-status.md](./problem/getrusage03-rusage-proc-status.md) 与 [problem/fsconfig-syscall.md](./problem/fsconfig-syscall.md)。
- **关联 commit**：`a2fe943`

#### getrusage03 zombie stat 与 timeout 卡死修复（6.12）

- **工具/模型**：Codex (GPT-5)
- **场景**：Bug 分析与定位、代码修复、文档完善
- **描述**：用户继续反馈 `getrusage03` 根据 `log.ans` 直接卡死不会自动结束。AI 复现并定位到两处后续问题：1) `/proc/<pid>/stat` 创建后状态固定为 `S`，父进程轮询等待 zombie `Z` 时会死循环；2) LTP 通过 `setitimer(ITIMER_REAL)` 设置的 SIGALRM 只在当前运行任务路径检查，主进程阻塞在 `waitpid` 时 timeout 不会触发。AI 增加 `/proc/<pid>/stat` 动态刷新，timer interrupt 扫描所有任务 itimer，并在阻塞任务 SIGALRM 到期时唤醒 `interruptible` 等待。随后测例可推进到 6 项 TPASS，LTP timeout 能自行发 SIGKILL、打印 summary 并 shutdown；剩余 `consume 500` 的 500MiB 匿名 mmap 触页未在 30 秒内完成，记录为后续待查。详见 `ai.log` 2026-06-12 条目与 [problem/getrusage03-rusage-proc-status.md](./problem/getrusage03-rusage-proc-status.md)。
- **关联 commit**：`0dc1f49`

#### LoongArch getrusage03 分段物理内存适配（6.13）

- **工具/模型**：Codex (GPT-5)
- **场景**：Bug 分析与定位、架构适配、文档完善
- **描述**：用户提供新的 LoongArch `log.ans` 并指出 RISC-V 的 QEMU 参数修复不能直接套用到 LoongArch。AI 复查 RISC-V 修复记录，确认 RISC-V 1GiB RAM 仍是连续区间；随后根据 LoongArch QEMU `virt` 设备树分析出 `-m 1G` 实际为低端 256MiB + 高端 768MiB 两段 RAM。人工审核后采纳 LoongArch 专用分段修复：QEMU 内存提升到 1GiB，`memory_layout.rs` 增加 `PHYSICAL_MEMORY_RANGES`，CMA 在 LoongArch 下按 range 初始化，RISC-V 保持原连续内存代码路径。详见 `ai.log` 2026-06-13 条目与 [problem/loongarch-getrusage03-split-ram.md](./problem/loongarch-getrusage03-split-ram.md)。
- **关联 commit**：`018cbd3`

#### LTP 包装层 Summary 缺失修复（6.13）

- **工具/模型**：Codex (GPT-5)
- **场景**：Bug 分析与定位、测试输出适配、文档完善
- **描述**：用户反馈 musl LTP `munmap01` 没有正常输出 Summary，并指出最终跑分会运行 `test_musl_ltp()` 等批量入口。AI 检查 `log.ans` 后确认测例实际已 TPASS，`munmap` 后的 StorePageFault 是测试预期，SIGSEGV handler 正常执行并退出 0；问题在 LTP 包装层只打印 wait status，旧式 LTP 输出不会自动生成统一 Summary。AI 修改 `run_ltp_tests_musl*()` / `run_ltp_tests_glibc()`，按 LTP 退出类型累计 passed/failed/broken/skipped/warnings，并去掉测试名末尾 NUL。`make` 与 `timeout 90s make run` 通过，munmap01 输出 TPASS 后出现 Summary。详见 `ai.log` 2026-06-13 条目与 [problem/ltp-summary-wrapper.md](./problem/ltp-summary-wrapper.md)。
- **关联 commit**：`3efa213`

#### LTP Summary 按输出 TPASS 数量统计（6.14）

- **工具/模型**：Codex (GPT-5)
- **场景**：Bug 分析与定位、测试输出适配、文档完善
- **描述**：用户指出 Summary 的 `passed` 不应由返回值决定，而应按 LTP 输出中的 `TPASS` 数量统计。AI 检查包装层后确认旧逻辑只能从 wait status 得知测例是否整体 exit 0，无法反映一个测例内多条断言结果。AI 将 LTP 子进程 stdout/stderr 接入 pipe，由父进程边转发日志边扫描 `TPASS/TFAIL/TBROK/TCONF/TWARN` token，Summary 优先按输出 token 数累计，只有无 token 时才退回 wait status。`make` 通过；`timeout 90s make run` 验证 abort01 两条 TPASS 汇总为 `passed 2`，后续 mixed TPASS/TFAIL/TWARN 测例也按输出数量递增。详见 `ai.log` 2026-06-14 条目与 [problem/ltp-summary-wrapper.md](./problem/ltp-summary-wrapper.md)。
- **关联 commit**：`cea92bc`

#### wait402 /proc/sys/kernel/pid_max 缺失修复（6.14）

- **工具/模型**：Codex (GPT-5)
- **场景**：Bug 分析与定位、proc 兼容节点补齐、文档完善
- **描述**：用户提供 `log.ans`，其中 `wait402` 在读取 `/proc/sys/kernel/pid_max` 时因 `ENOENT` 直接 `TBROK`。AI 检查日志和 proc 初始化代码后确认 `/proc/sys/kernel` 已存在，但只创建了 `tainted`，缺少 `pid_max` 兼容节点。修复在启动初始化中创建 `/proc/sys/kernel/pid_max` 并写入 Linux 常见值 `4194304`；随后 `make` 通过，`timeout 90s make run` 单跑 `wait402` 输出 `TPASS`，Summary 为 `passed 1 failed 0 broken 0`。详见 `ai.log` 2026-06-14 条目与 [problem/wait402-pid-max-proc.md](./problem/wait402-pid-max-proc.md)。
- **关联 commit**：`c8137ca`

#### wait403 wait4(INT_MIN) ESRCH 修复（6.14）

- **工具/模型**：Codex (GPT-5)
- **场景**：Bug 分析与定位、wait4 errno 兼容修复、文档完善
- **描述**：用户提供新的 `log.ans`，其中 `wait403` 期望 `wait4` 返回 `ESRCH`，但 Ya2yOS 返回 `ECHILD`。AI 检查 `sys_waitpid()` 后确认 `pid <= -2` 会统一取反为 pgid，遇到 `i32::MIN` 时不可表示的 `-pid` 被 release 构建绕回，随后落入普通进程组等待并返回 `ECHILD`。修复为解析 wait selector 前对 `pid == i32::MIN` 直接返回 `ESRCH`。`make` 通过，`timeout 90s make run` 单跑 `wait403` 输出 `TPASS`，Summary 为 `passed 1 failed 0 broken 0`。详见 `ai.log` 2026-06-14 条目与 [problem/wait403-int-min-esrch.md](./problem/wait403-int-min-esrch.md)。
- **关联 commit**：`08994d9`

#### waitid 系统调用实现（6.14）

- **工具/模型**：Codex (GPT-5)
- **场景**：系统调用实现、LTP 兼容修复、文档完善
- **描述**：用户要求实现 `waitid` syscall。AI 检查 syscall 表后确认 `WaitId = 95` 已接入但 `sys_waitid()` 未完成；随后参考现有 `sys_waitpid()` 实现等待、信号中断、`WNOHANG`、`WNOWAIT` 和回收路径，并补齐 `SigInfo` 中 Linux/musl `SIGCHLD` 所需的 `si_status` 布局。`make` 通过，`timeout 150s make run` 单跑 `waitid01` 输出 5 项 TPASS。详见 `ai.log` 2026-06-14 条目与 [problem/waitid-syscall.md](./problem/waitid-syscall.md)。
- **关联 commit**：`8ac1b40`

#### waitid07 WSTOPPED / SIGCONT checkpoint 超时修复（6.14）

- **工具/模型**：Codex (GPT-5)
- **场景**：Bug 分析与定位、信号语义修复、文档完善
- **描述**：用户提供 `log.ans`，其中 `waitid07` 在 `TST_CHECKPOINT_WAIT` 超时。AI 先补齐默认停止信号、`TaskStatus::Stopped`、`waitid(WSTOPPED)` 和 `CLD_STOPPED` 返回，使前 5 项断言通过；随后用临时 futex 日志确认父子 futex key 一致但子进程因 pending `SIGCONT` 返回 `EINTR` 后二次等待。最终修复 `trap_return()` 只处理一个 pending signal 的问题，使默认 `SIGCONT` 在回用户态前被消费；同时修正 `MAP_SHARED` groupid 条件和 RISC-V shared mmap fault 权限。`make` 通过，`timeout 150s make run` 单跑 `waitid07` 输出 5 项 TPASS。详见 `ai.log` 2026-06-14 条目与 [problem/waitid07-stopped-sigcont.md](./problem/waitid07-stopped-sigcont.md)。
- **关联 commit**：`d38cae4`

#### waitid10 core dump 信号终止状态修复（6.15）

- **工具/模型**：Codex (GPT-5)
- **场景**：Bug 分析与定位、waitid 信号语义修复、文档完善
- **描述**：用户提供 `log.ans`，其中 `waitid10` 在 `SIGFPE` 触发 core dump 场景下得到 `si_status=136` 和 `si_code=CLD_EXITED`，而 LTP 期望 `si_status=SIGFPE` 和 `si_code=CLD_DUMPED`。AI 定位到默认信号终止路径只保存 `128 + signo` exit code，`waitid()` 无法区分普通退出和 core dump 信号终止。修复在 `ProcessMeta` 记录默认信号终止原因，`waitid()` 根据信号记录返回 `CLD_KILLED/CLD_DUMPED` 与原始信号号。`make` 通过，`timeout 90s make run` 单跑 `waitid10` 输出 5 项 TPASS。详见 `ai.log` 2026-06-15 条目与 [problem/waitid10-core-dumped.md](./problem/waitid10-core-dumped.md)。
- **关联 commit**：`cf7e2c1`

#### waitid08 WCONTINUED 事件修复（6.15）

- **工具/模型**：Codex (GPT-5)
- **场景**：Bug 分析与定位、waitid continued 事件语义修复、文档完善
- **描述**：用户提供新的 `log.ans`，其中 `waitid08` 的 `WSTOPPED` 部分已经 TPASS，但父进程进入 `waitid(WCONTINUED)` 后阻塞，子进程 checkpoint futex 超时。AI 定位到 `SIGCONT` 只恢复 stopped task，没有记录可由 `waitid(WCONTINUED)` 观察的 continued event。修复在 `ProcessMeta` 新增 `continued_signal`，`SIGCONT` 实际恢复 stopped task 时记录事件并唤醒父进程，`waitid()` 返回 `SIGCHLD / CLD_CONTINUED / SIGCONT`。`make` 通过，`timeout 90s make run` 单跑 `waitid08` 输出 10 项 TPASS。详见 `ai.log` 2026-06-15 条目与 [problem/waitid08-wcontinued.md](./problem/waitid08-wcontinued.md)。
- **关联 commit**：`2c3750b`

#### waitid11 SIGKILL 终止状态修复（6.15）

- **工具/模型**：Codex (GPT-5)
- **场景**：Bug 分析与定位、waitid killed 事件语义修复、文档完善
- **描述**：用户要求继续修复 `waitid11`。AI 检查日志后确认子进程被 `SIGKILL` 杀死后，`waitid(WEXITED)` 返回 `si_status=0` 和 `CLD_EXITED`，而 LTP 期望 `SIGKILL / CLD_KILLED`。根因是子进程阻塞在 `pause()`，被 `SIGKILL` 唤醒后可能从阻塞 syscall 路径退出，未经过 `handle_signal()` 中记录 `termination_signal` 的默认信号处理分支。修复在进程级信号投递时对默认 `Terminate/CoreDump` 信号立即记录 termination event。`make` 通过，`timeout 90s make run` 单跑 `waitid11` 输出 5 项 TPASS。详见 `ai.log` 2026-06-15 条目与 [problem/waitid11-sigkill-killed.md](./problem/waitid11-sigkill-killed.md)。
- **关联 commit**：`15c2b94f`

#### waitpid10 zombie PID 复用与进程组等待修复（6.15）

- **工具/模型**：Codex (GPT-5)
- **场景**：Bug 分析与定位、waitpid 语义修复、文档完善
- **描述**：用户提供新的 `log.ans` 和 LTP waitpid 源码路径，要求修复 `waitpid10`。AI 定位到 `Pid 8 not reaped` 的根因是 TCB drop 立即释放共享 TID/PID，zombie 进程尚未被父进程 wait 回收时 PID 8 被新 fork 复用并覆盖全局进程表；同时补齐基础 `pgid`、`setpgid/getpgid` 和 `waitpid(0)`/`waitpid(<-1)` 进程组过滤语义。`make` 通过，`timeout 90s make run` 单跑 `waitpid10` 输出 1 项 TPASS。详见 `ai.log` 2026-06-15 条目与 [problem/waitpid10-pid-reuse-pgid.md](./problem/waitpid10-pid-reuse-pgid.md)。
- **关联 commit**：`43ae73b`

#### waitpid13 WUNTRACED stopped child 修复（6.15）

- **工具/模型**：Codex (GPT-5)
- **场景**：Bug 分析与定位、waitpid 停止态语义修复、文档完善
- **描述**：用户提供新的 `log.ans`，其中 `waitpid13` 没有打印 LTP failure，而是 QEMU timeout；日志显示父进程阻塞在 `waitpid(..., WUNTRACED)`，子进程均已处理 `SIGSTOP`。AI 确认 stopped event 只接入了 `waitid(WSTOPPED)`，`sys_waitpid()` 未处理 `WUNTRACED`，因此父进程无法向 stopped child 发送 `SIGCONT`。修复后 `waitpid()` 返回 stopped child 并写入 `WIFSTOPPED/WSTOPSIG` 所需 status。`make` 通过，`timeout 90s make run` 单跑 `waitpid13` 输出 1 项 TPASS。详见 `ai.log` 2026-06-15 条目与 [problem/waitpid13-wuntraced-stopped.md](./problem/waitpid13-wuntraced-stopped.md)。
- **关联 commit**：`149a314`

#### access01 权限判断与 cleanup 卡死修复（6.15）

- **工具/模型**：Codex (GPT-5)
- **场景**：Bug 分析与定位、文件权限语义修复、mmap cleanup 修复、文档完善
- **描述**：用户要求分析 `log.ans` 卡住原因并修复。AI 先定位 `access01` 的 20 项 `TFAIL` 来自 `faccessat` 未按 real uid/gid 和 owner/group/other 分类权限判断；修复后测例内部 199 项 TPASS，但 Summary 后仍卡住。进一步用临时日志确认卡点在 cleanup 删除 `/dev/shm/ltp_access01_2` 后的 `munmap`，MAP_SHARED 写回已 unlink backing file 时进入 ext4 写路径不返回。最终修复 `faccessat` 权限判断、`unlinkat` cleanup 语义、lwext4 目录删除和 unlinked shared mmap 的 `munmap` 写回路径。`make` 通过，`timeout 120s make run` 输出 `passed 199 failed 0`、`GROUP END` 和 `shutdown!`。详见 `ai.log` 2026-06-15 条目与 [problem/access01-permission-cleanup.md](./problem/access01-permission-cleanup.md)。
- **关联 commit**：`843b466`

#### basic umount 相对挂载点路径修复（6.16）

- **工具/模型**：Codex (GPT-5)
- **场景**：Bug 分析与定位、mount/umount 路径语义修复、文档完善
- **描述**：用户要求分析 `log.ans` 并修复 basic 测试 `umount` 失败。AI 定位到 `mount("./mnt")` 成功后 `umount("./mnt")` 返回 `-22`，根因是 `sys_mount()` 将挂载点原样保存为相对路径，而 `sys_umount2()` 查表前会把同一参数解析为绝对路径。修复为 `sys_mount()` 写入挂载表前规范化目标挂载点，source 保持原样。`make` 通过，`timeout 120s make run` 中 basic-musl/basic-glibc 的 `test_mount` 与 `test_umount` 均返回 0。详见 `ai.log` 2026-06-16 条目与 [problem/basic-umount-relative-mountpoint.md](./problem/basic-umount-relative-mountpoint.md)。
- **关联 commit**：`7ba78ac`

#### LoongArch busybox-glibc mprotect 越界改权限修复（6.16）

- **工具/模型**：Codex (GPT-5)
- **场景**：Bug 分析与定位、内存权限语义修复、文档完善
- **描述**：用户要求根据 `log.ans` 修复 busybox-glibc 的 `Fatal glibc error: malloc.c:2589 (sysmalloc)`。AI 复查日志和 `mprotect` 路径，定位到 `MemorySetInner::mprotect()` 的 VMA 拆分使用 `[start_vpn, end_vpn)`，但 PTE 权限更新使用 `..=end_vpn`，会额外修改相邻页权限并破坏 glibc 堆相关页属性。修复为按右开区间更新 PTE。`make log` 通过，`timeout 120s make run` 中 busybox-glibc 输出 GROUP END 和 `shutdown!`，未再出现 malloc fatal。详见 `Docs/初赛文档/ai.log` 2026-06-16 条目与 [problem/loongarch-busybox-mprotect-range.md](./problem/loongarch-busybox-mprotect-range.md)。
- **关联 commit**：`7366584`

#### utime03 LOOP_CTL_GET_FREE 返回语义修复（6.16）

- **工具/模型**：Codex (GPT-5)
- **场景**：Bug 分析与定位、loop-control ioctl 兼容修复、文档完善
- **描述**：用户要求分析 `log.ans` 中 `utime03` 失败。AI 定位到 `/dev/loop-control` 的 `ioctl(LOOP_CTL_GET_FREE)` 返回 `EFAULT`，随后 LTP 打印 `Couldn't find free loop device` 和 `TBROK: Failed to acquire device`。根因是内核把 ioctl 第三个参数误当作输出指针，而 Linux 语义要求该命令直接以 ioctl 返回值返回空闲 loop 号。修复后 `make` 通过，`timeout 120s make run` 单跑 `utime03` 输出 1 项 TPASS，Summary 为 `passed 1 failed 0 broken 0`。详见 `Docs/初赛文档/ai.log` 2026-06-16 条目与 [problem/utime03-loop-ctl-get-free.md](./problem/utime03-loop-ctl-get-free.md)。
- **关联 commit**：`af05b8d`

#### cyclictest glibc affinity / mlock / clone3 修复（6.16）

- **工具/模型**：Codex (GPT-5)
- **场景**：Bug 分析与定位、调度/内存/线程 syscall 兼容修复、文档完善
- **描述**：用户要求分析并修复 `cyclictest_testcode.sh` 失败。AI 先定位到 `sched_getaffinity()` 成功返回 0 且未写用户 mask，导致 libnuma/cyclictest 报 `request to allocate mask for invalid number`；修复后继续推进，依次处理 no-op `mlock` 过强映射校验导致的 `Bad address`，以及 `clone3` 在未设置 `CLONE_PIDFD` 时误校验 `pidfd` 字段导致 glibc pthread 创建失败。最终 `make` 通过，`timeout 120s make run` 中 `NO_STRESS_P1/NO_STRESS_P8/STRESS_P1/STRESS_P8` 四项均 success。详见 `Docs/初赛文档/ai.log` 2026-06-16 条目与 [problem/cyclictest-glibc-affinity-clone3.md](./problem/cyclictest-glibc-affinity-clone3.md)。
- **关联 commit**：`416ee7c`

#### cyclictest musl scheduler stub 兼容修复（6.16）

- **工具/模型**：Codex (GPT-5)
- **场景**：Bug 分析与定位、动态链接兼容补丁、调度 syscall 语义修复、文档完善
- **描述**：用户要求修复 cyclictest 中 `unable to get scheduler parameters`。AI 先排除 `sched_getaffinity` 返回值误判，确认返回 0 会导致 libnuma CPU mask 推断失败；随后从测试镜像提取并反汇编 `/musl/cyclictest` 与 `/musl/lib/libc.so`，定位到 LoongArch musl libc 的 `sched_getparam/getscheduler/setparam/setscheduler` wrapper 是直接返回 `ENOSYS` 的 stub，内核 syscall 根本未被调用。修复为读取 `/musl/lib/libc.so` 时对相关 stub 做内存态兼容补丁，并补齐内核 `sched_getparam` 写回语义。`make` 通过，`timeout 120s make run` 中 musl cyclictest 四个子项均 success。详见 `Docs/初赛文档/ai.log` 2026-06-16 条目与 [problem/cyclictest-musl-sched-stub.md](./problem/cyclictest-musl-sched-stub.md)。
- **关联 commit**：`e3a76fb`

#### overview UML 当前项目结构更新（6.20）

- **工具/模型**：Codex (GPT-5)
- **场景**：架构文档同步、PlantUML 结构图更新
- **描述**：用户要求根据当前项目修改 `Docs/uml/01_overview/01_overview.iuml`。AI 对照 `os/src/main.rs`、`os/Cargo.toml`、`Makefile` 和 `os/src` 模块结构，更新外部依赖、QEMU 双架构平台、基础/架构/核心/领域/系统调用/陷入各层模块说明，修正 `id_allocator` 归属、LoongArch 平台说明、`syscall` 子模块和启动流程。已完成文本静态检查；当前环境未安装 `plantuml`，未进行图片渲染验证。详见 `Docs/初赛文档/ai.log` 2026-06-20 条目。
- **关联 commit**：`3f1138d`

#### 内存管理 UML 建模补充（6.20）

- **工具/模型**：Codex (GPT-5)
- **场景**：内存管理文档同步、PlantUML 结构图与流程图补充
- **描述**：用户要求补齐内核内存管理文档中的 UML 图，重点围绕 `MemorySet` 核心结构和 `mmap`、`munmap`、`mprotect` syscall 建模。AI 对照 `os/src/mm/memory_set/`、`os/src/mm/map_area.rs`、`os/src/mm/page_fault_handler.rs`、`os/src/mm/group.rs` 和 `os/src/syscall/mm/mmap.rs`，在 `Docs/uml/03_mm_mana/03_mm_mana.iuml` 中补充核心结构设计类图、mmap 系统顺序图、munmap 活动图、mprotect 活动图和缺页处理交互图，并在 `Docs/ya2yos/03 内存管理.md` 增加 UML 建模索引。当前环境未安装 `plantuml`，未进行 PNG 渲染验证。详见 `Docs/初赛文档/ai.log` 2026-06-20 条目。
- **关联 commit**：`5555934`

#### gettimeofday timeval 微秒字段修复（6.23）

- **工具/模型**：Codex (GPT-5)
- **场景**：Bug 分析与定位、时间 syscall 语义修复、文档完善
- **描述**：用户反馈 `tvsub(struct timeval *)` 中 `assert(tdiff->tv_usec >= 0)` 失败，失败前调用了 `GetTimeOfDay` 和 `GetRusage`。AI 检查时间 syscall 后确认 `getrusage` 返回的是微秒 `TimeVal`，而 `sys_gettimeofday()` 错误按 `Timespec` 写回纳秒字段，导致用户态 `timeval.tv_usec` 可能远大于 `1000000`。修复将 `GetTimeOfDay` 改为写回 `TimeVal`，并补齐 `tv == NULL` 与 timezone 指针处理。`make` 通过；`timeout 120s make run` 在当前 glibc lmbench 配置中未再出现时间断言或 panic，但外层 timeout 截断，未验证整套完整 PASS。详见 `Docs/初赛文档/ai.log` 2026-06-23 条目与 [problem/gettimeofday-timeval-usec.md](./problem/gettimeofday-timeval-usec.md)。
- **关联 commit**：`7f36d56`

#### iozone 文件 I/O 性能优化（6.24）

- **工具/模型**：Codex (GPT-5)
- **场景**：文件系统与块设备性能优化、iozone 验证、文档完善
- **描述**：用户要求提高当前内核 iozone 得分。AI 检查 lwext4、VFS 和 virtio block 路径，定位到 Disk 层对连续 512B 对齐 I/O 仍逐块提交，以及 lwext4 小文件 cache 阈值 1MiB 无法覆盖 `iozone -a -s 4m`。修复为 Disk 层批量提交连续块，并将 VFileCache 阈值提高到 4MiB。`make` 通过；`timeout 180s make run` 单跑 `iozone-musl` 输出 GROUP END 和 `shutdown!`。详见 `Docs/初赛文档/ai.log` 2026-06-24 条目与 [problem/iozone-io-performance.md](./problem/iozone-io-performance.md)。
- **关联 commit**：`d076bb6`

#### signal01 SIGKILL/SIGSTOP 与 pause/ppoll 修复（6.25）

- **工具/模型**：Codex (GPT-5)
- **场景**：Bug 分析与定位、信号 syscall 语义修复、文档完善
- **描述**：用户要求检查 `log.ans` 调用路径，并解释为什么 `make` 与 `make log` 生成的 kernel 行为不同。AI 定位到两处根因：`rt_sigaction(SIGKILL, act, old_act)` 在 `act.sa_handler == SIG_DFL` 时被错误放行，导致 `signal(SIGKILL, ...)` 返回成功并触发 LTP `TFAIL`；glibc `pause()` 进入的 `ppoll(NULL, 0, NULL, NULL)` 路径未被正确支持，也不能在 pending signal 到来时返回 `EINTR`，因此 warn 构建下会卡在等待路径。`make log` 只是 debug 日志改变了调度时序并掩盖问题。修复后 `make`、`make log` 通过，单跑 `signal01` 输出 6 项 TPASS，Summary 为 `passed 6 failed 0`。详见 `Docs/初赛文档/ai.log` 2026-06-25 条目与 [problem/signal01-sigkill-sigaction.md](./problem/signal01-sigkill-sigaction.md)。
- **关联 commit**：`09d1b35`

#### iperf-musl 网络兼容修复（6.27）

- **工具/模型**：Codex (GPT-5)
- **场景**：Bug 分析与定位、网络 syscall/UDP 分发语义修复、文档完善
- **描述**：用户要求分析 `log.ans` 并修复 iperf 测试。AI 先根据日志修复 RISC-V 非规范用户 fault panic、`/dev/urandom` 缺失、`select/pselect6` fdset 和 ready 计数错误、`UserBuffer::read()` 造成的 TCP cookie 短写语义错误，以及 `SO_SNDBUF/SO_RCVBUF` 和 `getsockopt` ABI 问题。随后定位最终 `PARALLEL_UDP` timeout：smoltcp UDP ingress 只按本地端口投递到第一个 socket，而 Ya2yOS 上层 connected UDP recv 会按远端过滤，导致 `iperf -u -P 5` 多流数据进入错误队列。修复为底层 UDP socket 记录 remote endpoint，ingress 先匹配 connected 四元组，再退回普通监听 socket。`make` 通过，`timeout 300s make run` 中 `iperf-musl` 六个子项均 success。详见 `Docs/初赛文档/ai.log` 2026-06-27 条目与 [problem/iperf-musl-network-fixes.md](./problem/iperf-musl-network-fixes.md)。
- **关联 commit**：`a5ebace`

#### iperf-glibc daemon fstatat 与 /dev/null 修复（6.27）

- **工具/模型**：Codex (GPT-5)
- **场景**：Bug 分析与定位、glibc daemon 兼容修复、文档完善
- **描述**：用户提供新的 `log.ans`，其中 `iperf3 -s -D` 输出 `unable to become a daemon: Invalid argument`，导致后续客户端全部 `Connection refused`。AI 读取镜像脚本并反汇编 glibc 静态 `iperf3`，确认 `daemon()` 通过 `fstatat(fd, "", ..., AT_EMPTY_PATH)` 检查 `/dev/null`。根因是内核 `sys_fstatat()` 不支持 `AT_EMPTY_PATH` 空路径 fd 查询，且 `/dev/null` 的 `st_rdev` 不是 glibc 期望的 Linux `makedev(1,3)=259`。修复后 `make` 通过，`timeout 120s make run` 中 `iperf-glibc` 六个子项均 success。详见 `Docs/初赛文档/ai.log` 2026-06-27 条目与 [problem/iperf-glibc-daemon-fstatat-devnull.md](./problem/iperf-glibc-daemon-fstatat-devnull.md)。
- **关联 commit**：`faf995e`

#### LoongArch iperf-glibc statx 设备号修复（6.27）

- **工具/模型**：Codex (GPT-5)
- **场景**：Bug 分析与定位、LoongArch glibc `statx` 兼容修复、文档完善
- **描述**：用户要求根据新的 `log.ans` 修复 LoongArch 的 iperf 测试。AI 扫描日志确认 `iperf3 -s -D` 失败为 `unable to become a daemon: No such device`，随后反汇编 LoongArch 静态 `iperf3`，确认该架构 glibc 的 `fstat(fd)` wrapper 走 `statx(291)` 并由 glibc 将 `statx` major/minor 转回 `struct stat.st_rdev`。根因是内核 `kstat_to_statx()` 把已经编码的 `st_rdev=259` 直接放入 `stx_rdev_minor`，导致 glibc 重新组合后不等于 `/dev/null` 的 Linux `makedev(1,3)`。修复后 `make TARGET_ARCH=loongarch64` 通过，`timeout 120s make run` 中 `iperf-glibc` 六个子项均 success。详见 `Docs/初赛文档/ai.log` 2026-06-27 条目与 [problem/iperf-glibc-daemon-fstatat-devnull.md](./problem/iperf-glibc-daemon-fstatat-devnull.md)。
- **关联 commit**：`b7221de`

#### lmbench-musl lat_sig 死循环修复（6.27）

- **工具/模型**：Codex (GPT-5)
- **场景**：Bug 分析与定位、`pselect6`/`rt_sigaction` ABI 修复、mmap fault 权限语义修复、文档完善
- **描述**：用户要求分析 `log.ans` 并修复 iperf 优化后 lmbench 死循环。AI 先定位到 `lat_sig catch` 中父进程因 `pselect6` timeout 未回写 fd_set，误把旧 `exceptfds` bit 当作 pipe 异常并 cleanup；同时修复 raw `pselect6` sigmask 参数和 `rt_sigaction` raw ABI。继续验证后发现 `lat_sig prot` 卡住，根因是 mmap lazy fault 对 `PROT_READ` 映射的 store fault 未检查 `MapPermission::W`，错误补页导致收不到 `SIGSEGV`。修复后 `make`、`make log` 通过，`timeout 300s make run` 输出 `#### OS COMP TEST GROUP END lmbench-musl ####` 和 `shutdown!`。详见 `Docs/初赛文档/ai.log` 2026-06-27 条目与 [problem/lmbench.md](./problem/lmbench.md)。
- **关联 commit**：`6ababad`

#### RISC-V rt_sigaction restorer 取指 0 地址修复（6.27）

- **工具/模型**：Codex (GPT-5)
- **场景**：Bug 分析与定位、RISC-V signal ABI 修复、文档完善
- **描述**：用户要求优先处理 RISC-V 运行后在 `basic-musl` 处反复 `FetchInstructionPageFault bad addr=0x0` 的问题。AI 通过临时 trap/clone/exec/signal 诊断确认 PID 2 fork 与 exec busybox 均正常，真正跳到 0 发生在 SIGCHLD handler 返回后；根因是 RISC-V 用户态 `rt_sigaction` raw ABI 带 `sa_restorer`，而内核按 LoongArch 路径解析为 `handler, flags, mask[2], unused`，导致 `SA_RESTORER` handler 的返回地址 `ra` 被设为 0。修复后 `make` 通过，`timeout 80s make run` 已越过 basic/busybox/lua/iperf/cyclictest，未再出现该取指 fault。详见 `Docs/初赛文档/ai.log` 2026-06-27 条目与 [problem/riscv-sigaction-restorer-fetch-fault.md](./problem/riscv-sigaction-restorer-fetch-fault.md)。
- **关联 commit**：`966a1a8`

#### netperf-musl select/SIGCHLD EINTR 修复（6.28）

- **工具/模型**：Codex (GPT-5)
- **场景**：Bug 分析与定位、`pselect6/ppoll` 信号语义修复、网络 poll 锁重入修复、文档完善
- **描述**：用户要求分析 `log.ans` 并修复 netperf 失败。AI 定位到 `UDP_STREAM` 成功后 `netserver` 因 `accept_connections: select failure: Interrupted system call` 退出，后续子项控制连接失败；根因是 `pselect6/ppoll` 将默认忽略的 `SIGCHLD` pending signal 直接返回为 `EINTR`。修复为消费忽略类信号后继续等待。随后用户提供 GDB backtrace，确认 `sys_pselect6` 持有 task inner 锁调用 TCP `file.poll()`，loopback wake 路径再锁当前 TCB 导致重入 panic；修复为 fd poll 阶段不持有 task inner 锁。`make` 通过，单跑 netperf 前四项 success，剩余 `TCP_CRR` 的 `errno 9` 失败为后续问题。详见 `Docs/初赛文档/ai.log` 2026-06-28 条目与 [problem/netperf-select-sigchld-eintr.md](./problem/netperf-select-sigchld-eintr.md)。
- **关联 commit**：`8b90e3a`

#### netperf TCP_CRR blocked itimer 修复（6.28）

- **工具/模型**：Codex (GPT-5)
- **场景**：Bug 分析与定位、调度与 `setitimer` 唤醒修复、netperf 验证、文档完善
- **描述**：用户说明 `errno 92` 已消失，要求继续修复最后一个 `TCP_CRR` 失败。AI 根据 `log.ans` 定位到最后一次数据连接已完成，但客户端控制连接 `pselect6` 先超时，服务端阻塞在下一次 `accept` 的线程稍后才被 `SIGALRM` 打断并发送 656 字节结果。根因是内核态自愿调度期间没有检查 blocked task 的 itimer，导致 blocked `accept` 的 `SIGALRM` 唤醒滞后。修复为调度主循环只检查 `TaskStatus::Blocked` 任务的 timer，避免改变 running/ready 线程自身 `SIGALRM` 交付时机。`make` 通过，`timeout 300s make run` 中 netperf 五个子项均 success。详见 `Docs/初赛文档/ai.log` 2026-06-28 条目与 [problem/netperf-tcp-crr-blocked-itimer.md](./problem/netperf-tcp-crr-blocked-itimer.md)。
- **关联 commit**：`1a394cb`

#### pselect6 阻塞化实现（6.28）

- **工具/模型**：Codex (GPT-5)
- **场景**：系统调用语义增强、I/O 复用阻塞等待、waker/timer 调度修复、netperf 回归验证、文档完善
- **描述**：用户要求根据 `wait4` 的实现，将仍在轮询的 `pselect6` 改为可阻塞等待。AI 将 `sys_pselect6` 改为 `block_on(poll_fn(...))` 模式：先 poll fd，就绪则回写 fdset；未就绪时处理 pending signal、注册 fd waker，并用 timeout future 处理超时。实现过程中修复了 `MyWaker` 对 Running 任务重复入队导致的 TCB 锁重入，并为 pselect 等待增加 `skip_blocked_itimer_check`，避免它被 TCP_CRR 兼容用的 blocked itimer 扫描提前投递 SIGALRM。`make` 通过，`timeout 300s make run` 中 netperf 五个子项均 success。详见 `Docs/初赛文档/ai.log` 2026-06-28 条目与 [problem/pselect6-blocking-wait.md](./problem/pselect6-blocking-wait.md)。
- **关联 commit**：`d6b2549`

#### pipe pselect6 register panic 修复（6.28）

- **工具/模型**：Codex (GPT-5)
- **场景**：Bug 分析与定位、pipe poll/waker 语义修复、文档完善
- **描述**：用户提供 GDB backtrace，显示 `sys_pselect6()` 在监听 pipe fd 时调用 `File::register< Pipe >()` 并落到默认 `unimplemented!`。AI 检查 `Pipe` 与已有 eventfd/socket register 实现，定位到 pipe 只有同步阻塞 waiter 队列，没有 future-waker 路径。修复为在 pipe ring buffer 中增加读写 `PollSet`，实现 `File::register()`，在读写状态变化和端点关闭时唤醒等待者，并让读端全部关闭后的写入返回 `EPIPE`。`make` 通过，`timeout 120s make run` 未再出现原 `File::register` panic。详见 `Docs/初赛文档/ai.log` 2026-06-28 条目与 [problem/pipe-pselect-register-panic.md](./problem/pipe-pselect-register-panic.md)。
- **关联 commit**：`1af1639`

#### ya2yos 第八章 AI 使用情况文档润色（6.29）

- **工具/模型**：Codex (GPT-5)
- **场景**：项目文档润色、AI 使用情况总结
- **描述**：用户要求润色 `Docs/ya2yos` 第八章。AI 对照相邻章节风格，将 `Docs/ya2yos/08 AI的使用情况.md` 从简短口语化说明整理为“使用概况、主要工作成果、使用中的不足、个人反思”四节，并补齐 `第八章`、`8.1` 至 `8.4` 的章节编号。本次仅修改文档，未涉及内核代码和测试运行。详见 `Docs/初赛文档/ai.log` 2026-06-29 条目。
- **关联 commit**：`d5f465`

#### page fault 统一入口重构（6.29）

- **工具/模型**：Codex (GPT-5)
- **场景**：缺页处理路径重构、命名语义修正、双架构构建验证、文档完善
- **描述**：用户指出 `valid PTE` 的写权限 fault 进入 `handle_cow_page_fault()` 容易造成语义误解，建议统一 page fault 入口。AI 将 trap 层和用户指针路径统一改为调用 `MemorySet::handle_page_fault()`，内部拆分为 not-present fault 与 present PTE write-protect fault；同时将外层 `cow_page_fault()` 和双架构页表 `handle_cow_page_fault()` 改名为 write-protect 语义，保持实际 PTE flags、refcnt、复制与 TLB 刷新逻辑不变。`make` 双架构通过，`timeout 120s make run` 未出现 page fault / SIGSEGV / panic 关键错误。详见 `Docs/初赛文档/ai.log` 2026-06-29 条目与 [problem/page-fault-unified-handler.md](./problem/page-fault-unified-handler.md)。
- **关联 commit**：`44e3291`

#### signal/itimer 职责重构（6.29）

- **工具/模型**：Codex (GPT-5)
- **场景**：信号与 timer 模块边界重构、`pselect6` 兼容字段移除、netperf 回归修复、文档完善
- **描述**：用户要求移除此前为 `pselect6` 修复引入的 `TaskControlBlockInner::skip_blocked_itimer_check`，并将信号处理职责从 TCB 移交给 timer 和 signal 模块。AI 将 itimer 到期推进放入 `Timer::take_expired_signal()`，将 `SIGALRM` 投递放入 `signal::deliver_itimer_signal()` / `deliver_blocked_itimer_signal()`，删除 TCB 字段和 `TaskControlBlock::check_timer()`；根据最新 `log.ans` 继续定位 `TCP_STREAM errno 4` 回退，确认根因是 `interruptible()` 正常完成后残留 `interrupt_waker`，导致后续 `pselect6` 被 blocked itimer 补扫误唤醒。修复后 `make` 通过，单跑 `netperf-musl` 五个子项均 success，未再出现 `recv_response_timed_n` 或 `errno 4/9/92`。详见 `Docs/初赛文档/ai.log` 2026-06-29 条目与 [problem/signal-itimer-refactor.md](./problem/signal-itimer-refactor.md)。
- **关联 commit**：`d44f2dd`

#### acct02 process accounting 修复（6.30）

- **工具/模型**：Codex (GPT-5)
- **场景**：Bug 分析与定位、LTP `acct02` 兼容修复、文档完善
- **描述**：用户要求分析 `log.ans` 失败原因并修复内核。AI 根据日志确认 `acct02` 首先因缺少可读取的 kernel config 在 `tst_kconfig` 阶段 `TBROK`，随后结合 LTP 源码确认测例还会验证 `acct(2)` 写出的旧版 `struct acct` 记录。修复补齐 `/boot/config-5.0.0`、实现进程退出时写 accounting 记录、维护进程 `comm`，并避免正常 `exit_group()` 的内部 SIGKILL 覆盖真实终止原因。`make` 通过，LoongArch musl 单跑 `acct02` 输出 1 项 TPASS。详见 `Docs/初赛文档/ai.log` 2026-06-30 条目与 [problem/acct02-process-accounting.md](./problem/acct02-process-accounting.md)。
- **关联 commit**：待提交

#### bind02 getgrgid 与特权端口修复（6.30）

- **工具/模型**：Codex (GPT-5)
- **场景**：Bug 分析与定位、LTP `bind02` 兼容修复、文档完善
- **描述**：用户要求分析 `log.ans` 并修复失败。AI 根据日志确认 musl/glibc `bind02` 均在 setup 阶段因 `getgrgid(0)` 返回 `ENOENT` 而 `TBROK`；结合 LTP 源码和 `initfiles` 确认 `/etc/passwd` 中 `nobody` 的 gid 为 0，但系统未创建 `/etc/group`。继续检查 `bind()` 语义后发现 TCP/UDP 缺少 1024 以下特权端口权限检查。修复补齐最小 `/etc/group`，并让非 root 绑定特权端口返回 `EACCES`。`make`、`make log` 通过，LoongArch 单跑 musl/glibc `bind02` 均输出 `TPASS: bind() : EACCES (13)`。详见 `Docs/初赛文档/ai.log` 2026-06-30 条目与 [problem/bind02-privileged-port.md](./problem/bind02-privileged-port.md)。
- **关联 commit**：待提交

#### execv01 MAP_SHARED 文件页共享修复（6.30）

- **工具/模型**：Codex (GPT-5)
- **场景**：Bug 分析与定位、mmap `MAP_SHARED` 语义修复、LTP execv 回归验证、文档完善
- **描述**：用户要求分析 `log.ans` 中 `tst_test.c:1449: TBROK: Test haven't reported results!`。AI 根据日志确认 `execv01_child` 已打印 `TPASS`，但父 LTP 框架的共享结果计数没有变化；结合 LTP `tst_reinit()` 逻辑确认 exec 后子程序会重新打开 `LTP_IPC_PATH` 并 `mmap(MAP_SHARED)` 同一结果文件。根因是 Ya2yOS 原 `MAP_SHARED` 仅按 fork 继承的 `MapArea.groupid` 共享，没有按文件页共享，导致 exec 后重新 mmap 的结果页与父进程分裂。修复为在 `GROUP_SHARE` 中增加 `(path, page_index)` 文件页缓存，文件 `MAP_SHARED` 缺页时复用同一 `FrameTracker`。`make` 通过，LoongArch 单跑 musl/glibc `execv01` 均输出 `passed 1 failed 0 broken 0`。详见 `Docs/初赛文档/ai.log` 2026-06-30 条目与 [problem/execv01-mmap-shared-reinit.md](./problem/execv01-mmap-shared-reinit.md)。
- **关联 commit**：待提交

#### execve02 执行权限检查修复（6.30）

- **工具/模型**：Codex (GPT-5)
- **场景**：Bug 分析与定位、`execve` 权限语义修复、LTP execve 回归验证、文档完善
- **描述**：用户要求分析 `log.ans` 中 `execve_child.c:27: TFAIL: execve_child shouldn't be executed`。AI 读取 LTP `execve02.c` 确认测试将 `execve_child` 改成 `0700` 后切换到 `nobody` 执行，期望 `execve()` 返回 `EACCES`；检查内核发现 `sys_execve()` 只用 `O_RDONLY` 打开并读取 ELF，缺少基于 effective uid/gid 的执行位检查。修复后 `execve` 在读取目标 ELF 前检查 `S_IXUSR/S_IXGRP/S_IXOTH`，shebang 解释器也走同一检查。`make` 通过，LoongArch 单跑 musl/glibc `execve02` 均输出 `TPASS: execve() failed expectedly: EACCES (13)`。详见 `Docs/初赛文档/ai.log` 2026-06-30 条目与 [problem/execve02-exec-permission.md](./problem/execve02-exec-permission.md)。
- **关联 commit**：待提交

#### execve04 ETXTBSY 修复（6.30）

- **工具/模型**：Codex (GPT-5)
- **场景**：Bug 分析与定位、`execve`/普通文件写打开语义修复、LTP execve 回归验证、文档完善
- **描述**：用户提供新的 `log.ans`，要求继续修复 `execve_child.c:27: TFAIL: execve_child shouldn't be executed`。AI 确认新失败为 `execve04`，读取 LTP 源码后定位到该用例要求目标可执行文件被另一个进程 `O_WRONLY` 打开时，`execve()` 返回 `ETXTBSY`。根因是内核没有跨进程记录普通文件写打开状态，`sys_execve()` 无法发现子进程持有的写 fd。修复为在 `OSFile` 生命周期中维护 inode path 写打开计数，并在 `execve` 读取 ELF 前返回 `ETXTBSY`。`make` 通过，LoongArch 单跑 musl/glibc `execve04` 均输出 `TPASS: execve failed as expected: ETXTBSY (26)`。详见 `Docs/初赛文档/ai.log` 2026-06-30 条目与 [problem/execve04-etxtbsy.md](./problem/execve04-etxtbsy.md)。
- **关联 commit**：待提交

#### execve06 空 argv 修复（6.30）

- **工具/模型**：Codex (GPT-5)
- **场景**：Bug 分析与定位、`execve` argv 兼容语义修复、LTP execve 回归验证、文档完善
- **描述**：用户要求分析新的 `log.ans` 并修复。AI 确认当前失败为 `execve06`：musl 子进程因空 argv 触发 SIGSEGV，glibc 子程序报告 `argc is 0, expected 1`。读取 LTP 源码后确认测试传入 `argv = { NULL }`，要求内核补充 dummy `argv[0]`。根因是 `sys_execve()` 在 `argv[0] == NULL` 时保持 `argv_vec` 为空，导致新程序得到 `argc=0`。修复为 argv 解析后若为空则补充空字符串作为 `argv[0]`。`make` 通过，LoongArch 单跑 musl/glibc `execve06` 均输出 `TPASS: argv[0] was filled in by kernel`。详见 `Docs/初赛文档/ai.log` 2026-06-30 条目与 [problem/execve06-empty-argv.md](./problem/execve06-empty-argv.md)。
- **关联 commit**：待提交

#### kill11 wait status core dump bit 修复（6.30）

- **工具/模型**：Codex (GPT-5)
- **场景**：Bug 分析与定位、wait/waitpid 信号终止状态修复、LTP kill 回归验证、文档完善
- **描述**：用户要求分析 `log.ans` 并修复。AI 确认当前失败为 `kill11`：非 core dump 的默认终止信号也被用户态 `WCOREDUMP(status)` 识别为 core dump。读取 LTP 源码和内核信号路径后定位到 `sys_waitpid()` 直接返回内部 `exit_code = signo + 128`，该值无条件带 0x80；内核已有 `termination_signal=(signo, dumped_core)` 元数据但 waitpid 未使用。修复为 wait status 按 `signo | (dumped_core ? 0x80 : 0)` 编码，普通 exit 仍左移退出码。`make` 通过，LoongArch 单跑 musl/glibc `kill11` 的 LTP Summary 均为 `passed 24 failed 0 broken 0`。详见 `Docs/初赛文档/ai.log` 2026-06-30 条目与 [problem/kill11-wait-core-status.md](./problem/kill11-wait-core-status.md)。
- **关联 commit**：待提交

#### lseek02 fd 错误码与 FIFO ESPIPE 修复（6.30）

- **工具/模型**：Codex (GPT-5)
- **场景**：Bug 分析与定位、`lseek` 错误码语义修复、VFS 特殊节点类型兼容、LTP 回归验证、文档完善
- **描述**：用户要求分析新的 `log.ans` 并继续修复。AI 确认当前失败为 `lseek02`：无效 fd 返回了 `EINVAL` 而非 `EBADF`，匿名 pipe 返回 `EINVAL` 而非 `ESPIPE`，命名 FIFO 被错误允许 seek。修复为 `sys_lseek()` 先通过 fd 表返回 `EBADF`，`File::lseek()` 默认返回 `ESPIPE`，并在 `FsIndex` 中登记 `mknodat()` 创建的特殊节点类型，使命名 FIFO 在 `OSFile::lseek()` 中返回 `ESPIPE`。`make` 通过，LoongArch 单跑 musl/glibc `lseek02` 的 LTP Summary 均为 `passed 15 failed 0 broken 0`。详见 `Docs/初赛文档/ai.log` 2026-06-30 条目与 [problem/lseek02-fd-espipe-fifo.md](./problem/lseek02-fd-espipe-fifo.md)。
- **关联 commit**：待提交

#### pread02 pipe/目录错误码修复（6.30）

- **工具/模型**：Codex (GPT-5)
- **场景**：Bug 分析与定位、`pread64/pwrite64` 错误码语义修复、LTP 回归验证、文档完善
- **描述**：用户要求分析新的 `log.ans` 并继续修复。AI 确认当前失败为 `pread02`：pipe fd 的 `pread()` 返回 `EINVAL` 而非 `ESPIPE`，目录 fd 的 `pread()` 错误成功。修复为 `sys_pread64()` 通过通用 fd 对象调用 `lseek` 判定不可 seek fd，并对目录 fd 显式返回 `EISDIR`；同时同步修正同源 `sys_pwrite64()` 的无效 fd、只读 fd 和不可 seek fd 错误码顺序。`make` 通过，LoongArch 单跑 musl/glibc `pread02` 的 LTP Summary 均为 `passed 3 failed 0 broken 0`。详见 `Docs/初赛文档/ai.log` 2026-06-30 条目与 [problem/pread02-pipe-dir-errors.md](./problem/pread02-pipe-dir-errors.md)。
- **关联 commit**：待提交

#### preadv2/pwritev2 系统调用实现（6.30）

- **工具/模型**：Codex (GPT-5)
- **场景**：系统调用实现、raw ABI 参数处理、iovec 读写语义、LTP 回归验证、文档完善
- **描述**：用户要求实现 `pwritev2` 和 `preadv2`。AI 对照 LTP wrapper 与用例，确认 raw ABI 需要合并 `pos_l/pos_h` 并独立处理 flags，且旧号 `preadv/pwritev` 也需要复用 flags=0 的后端。修复实现了 `offset=-1` 使用当前 offset、显式 offset 不改变当前 offset、iovec 数量/长度/总长度校验、64KiB 分片搬运和非零 flags 返回 `EOPNOTSUPP`；同时新增无副作用 `probe_user_write()`，避免读入前探测用户缓冲区时污染数据。`make` 在 LoongArch 通过，临时单跑 musl/glibc `preadv201/202`、`pwritev201/202` 均为 `failed 0`，恢复测试入口后 `make` 再次通过，`make TARGET_ARCH=riscv64` 编译通过。详见 `Docs/初赛文档/ai.log` 2026-06-30 条目与 [problem/preadv2-pwritev2-syscalls.md](./problem/preadv2-pwritev2-syscalls.md)。
- **关联 commit**：待提交
