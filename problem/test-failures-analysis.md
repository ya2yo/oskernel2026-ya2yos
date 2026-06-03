# LTP / libctest 测试失败分析

> 基于 log.ans 和 GDB 调试结果，2025-06-03

## 一、已修复的内核 Panic（2 个）

### 1. 堆分配器 Panic — ppoll 无上限 nfds

**现象**:
```
[kernel] Panicked at src/mm/heap_allocator.rs:18
Heap allocation error, layout = Layout { size: 881511176, align: 1 }
```

**根因**: crash02 fuzzer 测试调用 `sys_ppoll` 时传入 `nfds = 0x6915961` (~1.1 亿)，
内核直接 `vec![0u8; nfds * sizeof(PollFd)]` 尝试分配 ~840MB 内存。

**修复**: `os/src/syscall/io_mpx/poll.rs` — nfds 上限检查
```rust
let nfds = min(nfds, proc_inner.fd_table.get_soft_limit());
```

同时预防性修复 `os/src/syscall/fs/ctl.rs` — readlinkat bufsize 上限

### 2. 页表翻译 Panic — 信号栈溢出

**现象**:
```
[kernel] Panicked at src/mm/translate.rs:244
called `Option::unwrap()` on a `None` value
#5  put_data (ptr=0x57) → translate_va 返回 None
#6  setup_frame (signo=32, SIGRTMIN)
#7  handle_signal
#8  trap_return
```

**根因**: `setup_frame` 向用户栈写入信号帧数据时，未检查栈是否有足够空间。
当用户栈接近耗尽时，`user_sp - sizeof(UserContext)` 落在未映射区域，
`put_data` 中的 `translate_va().unwrap()` 触发 panic。

**修复**: `os/src/signal/mod.rs` — 添加栈空间检查
```rust
let stack_bottom = task_inner.user_stack_top - USER_STACK_SIZE;
if user_sp < stack_bottom + min_frame_size {
    // 栈不足，安全终止进程而非内核 panic
    exit_current_and_run_next((signo + 128) as i32);
}
```

## 二、已修复的功能性 Bug（4 个）

### 3. LTP shell 脚本找不到库脚本 (tcp_cc_lib.sh 等)

**现象**: 大量 LTP shell 脚本报 `. tcp_cc_lib.sh: not found` 等错误

**根因**: 内核默认 PATH=/bin，shell 的 `.` 命令只搜 PATH，不搜当前目录。
LTP 库脚本在 `/musl/ltp/testcases/bin/`，不在 PATH 中。

**修复**: `os/src/syscall/task/execve.rs` — PATH 添加当前目录
```rust
env.push("PATH=/bin:.".to_string());
```

### 4. 动态库路径重定向未接入 open 系统调用

**现象**: 动态链接器请求 `libgcc_s.so.1` 等库时找不到文件

**根因**: `map_dynamic_link_file` 函数已实现路径重定向，但从未在 `sys_openat` 中被调用。
动态链接器的 `open()` 直接走文件系统，不经过路径映射。

**修复**: `os/src/syscall/fs/fd_ops.rs` — 接入 map_dynamic_link_file
```rust
let abs_path = map_dynamic_link_file(&abs_path).to_string();
```
并过滤非库文件路径（排除 ld.so.preload、ld.so.cache 等）

### 5. libm.so.6 找不到

**现象**:
```
map_dynamic_link_file cannot find DL path for path:/lib/riscv64-linux-gnu/libm.so.6
```

**根因**: 磁盘镜像只有 `libm.so`，但 RISC-V glibc 动态链接器请求 `libm.so.6`（带版本后缀），
DYNAMIC_PATH 中未注册 `libm.so.6`。

**修复**: 
- `os/src/fs/map_dynamic_link.rs` — 注册 `/glibc/lib/libm.so.6`
- `os/src/fs/kernel_fs_ops/initfiles.rs` — 启动时创建软链接

### 6. libgcc_s.so.1 缺失（竞赛固定镜像问题）

**根因**: 竞赛磁盘镜像不含 `libgcc_s.so.1`，但 glibc pthread 测试依赖它。

**修复**: 
- 新建 `os/src/arch/*/qemu/asms/libgcc_preload.S` — 编译期内嵌二进制
- `os/src/fs/kernel_fs_ops/initfiles.rs` — 启动时写入 /glibc/lib/libgcc_s.so.1
- 需要编译环境中存在 `/usr/riscv64-linux-gnu/lib/libgcc_s.so.1`

## 三、glibc-libctest 失败清单（19 项）

| 测试名 | 失败原因 | 分类 |
|--------|----------|------|
| `clocale_mbfuncs` | wcrtomb/wcsrtombs locale 功能不完整 | locale/C库 |
| `fnmatch` | 复杂 glob 模式匹配不支持 | C库 |
| `fscanf` | scanf 格式解析实现不完整 | C库 |
| `fwscanf` | wscanf 格式解析实现不完整 | C库 |
| `mbc` | 无法设置 UTF-8 locale (ANSI_X3.4-1968) | locale |
| `pthread_cancel_points` | 触发信号栈溢出 → kernel panic | **已修复** |
| `sscanf` | scanf 格式解析实现不完整 | C库 |
| `sscanf_long` | scanf long 格式解析实现不完整 | C库 |
| `strftime` | strftime 格式化实现不完整 | C库 |
| `strtol` | strtol 实现不完整 | C库 |
| `swprintf` | wide printf 触发 SIGABRT | C库 |
| `wcstol` | wide strtol 实现不完整 | C库 |
| `printf_fmt_n` | printf %n 格式触发 SIGABRT | C库 |
| `setvbuf_unget` | setvbuf 后 unget 触发 SIGSEGV | C库 |
| `daemon_failure` | daemon() 实现不完整 | 系统调用 |
| `dn_expand_empty` | DNS 解析器实现不完整 | 网络 |
| `dn_expand_ptr_0` | DNS 解析器实现不完整 | 网络 |
| `fgetwc_buffering` | wide char 缓冲 IO 实现不完整 | C库 |
| `regex_ere_backref` | 正则表达式反向引用不支持 | C库 |
| `regex_escaped_high_byte` | 正则表达式高位字节转义不支持 | C库 |

## 四、LTP 测试失败（略，因 log 中 LTP 部分未成功运行）

LTP 测试主要因两类问题未跑完：
1. crash02 (syscall fuzzer) 触发 ppoll panic — **已修复**
2. 大量 shell 脚本因 PATH 问题找不到库脚本 — **已修复**

修复后需要重新运行验证。

## 五、遗留问题

1. **C 库完整性问题**（13 项）: scanf/printf/strftime/strtol/regex 等标准库实现不完整，
   需要完善 musl/glibc 的 locale 支持和格式化 IO
2. **DNS 解析器**（2 项）: dn_expand 函数实现不完整
3. **daemon() 系统调用**: 实现不完整
4. **/etc/ld.so.cache**: 动态链接器缓存文件缺失，不影响功能但有警告
