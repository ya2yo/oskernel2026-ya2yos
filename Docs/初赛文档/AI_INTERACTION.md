# AI 使用记录

## 声明

本项目在开发过程中使用了以下 AI 工具及大模型：

| 类别 | 名称 |
| ------ | ------ |
| AI 编程工具 | Cursor, Claude Code |
| 大语言模型 | GPT-5.5 (ChatGPT), Gemini 3 Flash Preview, DeepSeek-v4 |

所有 AI 工具的使用均限于**辅助开发**（代码分析、Bug 定位、代码生成建议、文档完善），最终代码决策由人工审核后采纳。按照大赛要求，本文档及开发日志、项目设计文档中均设有 AI 使用专门章节，git commit 记录中标注了 AI 使用情况。由于部分早期 commit 的 AI 标注不够完整，本文档作为最全面的补充说明。

---

## AI 使用场景分类

### 1. 代码理解与注释

- 迁移 StarryOS 网络模块时，使用大模型辅助阅读和理解复杂的异步网络代码，添加注释
- 为 `pci_config` 相关函数、`trap.S` 等底层汇编代码添加注释

### 2. 代码生成

- 生成 socket 系统调用的基础测试用例
- 辅助实现 `recvfrom`、`recvmsg`、`sendmsg` 等系统调用
- 辅助实现 `setsockopt`/`getsockopt`、Unix 类型套接字等功能
- 辅助重构 `initproc.rs` 结构
- 辅助完成了 `copy_from_user` 和 `copy_to_user` 的实现

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
- **场景**：代码理解
- **描述**：StarryOS 的网络模块采用异步编程模型，代码结构复杂。使用多个大模型辅助阅读代码，添加注释以便理解 `Future`、`block_on` 等异步机制。
- **关联 commit**：`6bafdb6`（merge starry 分支）

#### 生成 Socket 测试用例（5月初）

- **工具/模型**：ChatGPT
- **场景**：代码生成
- **描述**：利用 AI 生成 socket 系统调用（`sys_socket`、`sys_bind`、`sys_listen`、`sys_accept` 等）的基础测试用例。
- **关联 commit**：`ef5703d`, `7b447ed`

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

#### 线程5僵尸线程死循环（5.19）

- **工具/模型**：Cursor
- **场景**：Bug 分析
- **描述**：使用 Cursor 对输出的日志进行分析。发现线程5成为僵尸线程未能释放，导致内核死循环。根因是在引入 StarryOS 网络模块时未能充分理解异步编程思想，创建的 Future 里面对 Task 的强引用在不运行的情况下不会被释放。
- **关键发现**：日志中 `Pending strong_count = 4` 表明引用计数异常，导致 future 无法被 drop。
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
- **描述**：使用 Claude Code 完善开发日志（`开发日志.md`）、问题解决记录（`problem.md`）和本文档。
- **关联 commit**：`d2c20ba`, `31e3369`

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
