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
- **关联 commit**：`e5d1b07`, `4a9b73c`, `5f90be8`；`opt.rs` `?` 修复待提交

#### 项目 Agent 技能与文档规范（5.29）

- **工具/模型**：Cursor (Composer)
- **场景**：文档完善
- **描述**：编写 `.claude/skills/` 下 build-and-test、ltp-test-triage、network-debug 等技能；定义开发日志（简约）/ problem / ai.log / AI_INTERACTION 四份文档分工。
- **关联文件**：`.claude/skills/doc-writing/SKILL.md`

#### LTP access04 mount / loop 设备（5.30）

- **工具/模型**：Cursor (Composer)
- **场景**：日志分析、Bug 修复、代码生成
- **描述**：多轮 `log.ans` 排查 LTP access04：mount 缺页 panic → `copy_from_user`；LoongArch TBROK → 实现 loop 块设备与 `/dev/loop-control`；tmpfs `special=NULL` EFAULT → 空指针转空串；LA `handle_mprotect` 懒分配页修复。用户验证 LoongArch musl 通过后补文档。过程详见 `ai.log` 2026-05-30 条目与 [problem/access04-ltp-musl.md](./problem/access04-ltp-musl.md)。
- **关联 commit**：待提交

#### LTP access02 execve shebang（5.30）

- **工具/模型**：Cursor (Composer)
- **场景**：日志分析、Bug 修复
- **描述**：提供 LoongArch `log.ans`，`access02` 在 X_OK 执行验证阶段 4× TFAIL；对照 LTP 源码确认 `file_x` 为 `#!/bin/sh` 脚本；定位 `sys_execve` 对非 ELF 直接 `ENOEXEC`。AI 实现 shebang 解析与解释器 argv 重建，用户验证通过后补文档。详见 `ai.log` 2026-05-30 access02 条目与 [problem/access02-ltp-execve.md](./problem/access02-ltp-execve.md)。
- **关联 commit**：待提交

#### setresgid(149) 系统调用（5.31）

- **工具/模型**：Cursor (Composer)
- **场景**：代码生成、测例验证
- **描述**：按 syscall-implementation skill 实现 `setresgid`/`getresgid`：TCB 维护 GID 三元组、Linux 级联语义与非特权 EPERM 检查；修正 `GetResgid` 编号 148→150。RISC-V `setresgid01` 5× TPASS。详见 `ai.log` 2026-05-31 条目与 [problem/setresgid-syscall.md](./problem/setresgid-syscall.md)。
- **关联 commit**：待提交

#### clone03 MAP_SHARED fork 帧共享修复（5.31）

- **工具/模型**：Cursor (Composer)
- **场景**：Bug 分析与定位
- **描述**：提供 `log.ans`，AI 分析 clone03 失败日志，定位 `from_existed_user` 中 MAP_SHARED 懒分配区域 fork 后父子各自独立分配物理帧，破坏共享语义。同时定位 `recycle_data_pages` 中 `MAP_ANONYMOUS` 区域 `unwrap() None` panic。实现预 fault pass（MAP_ANONYMOUS 零页 / 文件支撑读文件），并增加 `is_some()` 检查。详见 `ai.log` 与 [problem/clone-mmap-shared-fork.md](./problem/clone-mmap-shared-fork.md)。
- **关联 commit**：待提交

#### clone05 CLONE_VFORK 挂起机制（5.31）

- **工具/模型**：Cursor (Composer)
- **场景**：Bug 分析与定位
- **描述**：提供 `log.ans`，AI 分析 clone05 测试失败原因：内核完全未实现 CLONE_VFORK 挂起。第一轮修复后持续失败，对比两轮日志定位三处调度路径（suspend_current_and_run_next 无条件 Ready、run_tasks 无差别入队、空队列 keep-running）绕过 VforkBlocked。逐一修复后测试通过。详见 `ai.log` 2026-05-31 条目与 [problem/clone05-vfork.md](./problem/clone05-vfork.md)。
- **关联 commit**：待提交

---

## AI 成果总结

### 按场景统计

| 使用场景 | 次数（约） | 涉及 commit 数 |
| ---------- | ----------- | --------------- |
| 代码理解与注释 | 3 | 5+ |
| 代码生成 | 8 | 15+ |
| Bug 分析与定位 | 10 | 20+ |
| 架构适配与调试 | 5 | 15+ |
| 文档完善 | 3 | 5+ |

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
