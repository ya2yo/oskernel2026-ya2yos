# BuildStorm epoll 注册表与跨进程 fd 复用

## 背景

`server.ans` 在 `BUILDSTORM_TOOLCHAIN`、`BUILDSTORM_MINIBUILD` 和 `tg-xtask` 预构建完成后，首次执行 Tokio runtime 初始化失败。该路径会创建 epoll、eventfd 并注册 eventfd，属于 BuildStorm 多进程并发构建的共同基础设施。

## 现象

日志中的 runtime 进程在 `tg-xtask` 启动后立即 panic：

```text
Failed building the Runtime: Os { code: 9, message: "Bad file descriptor" }
BUILDSTORM_COMPILE mode=multi ok=false rc=101
```

同一份日志中反复出现的 `ext4_stat_get: rc = 2` 来自 Cargo 并发清理已经删除的临时对象文件，不是该 panic 的根因。

## 分析

原实现以 raw `epoll fd` 为 key 写入全局 `EPOLL_TABLE`。fd 号只在进程内有意义；并发子进程可以同时分配相同的 fd 号，后注册的 epoll 实例会覆盖先注册的实例。先注册实例随后调用 `epoll_ctl` 或 `epoll_wait` 时，查到的对象可能已经属于另一个进程，最终被错误报告为 `EBADF`。

`FdTable` 中的 epoll 描述符保存的是 `Arc<dyn File>`，fork 或共享 fd 表会复制同一个 `Arc`。因此可以先从当前进程的 fd 表取得实际文件对象，再按 `Arc::ptr_eq` 与全局注册表中的 `EpollFile` 对象匹配。

## 根因

全局注册表错误地把进程局部的 raw fd 当作跨进程唯一标识，导致 fd 复用覆盖仍存活的 epoll 实例。

## 修复

- `EPOLL_TABLE` 改用 `EpollFile` 分配对象地址作为 key，保存 `Weak<EpollFile>`，不再使用 raw fd 作为全局标识。
- `EpollFile::lookup` 接收当前进程 `FdTable`，先解析 `epfd`，再用 `Arc::ptr_eq` 返回对应实例；无效或非 epoll 描述符仍返回错误。
- `epoll_ctl`、`epoll_pwait` 和单次等待路径统一通过新的 lookup 接口校验 epoll 实例。

## 涉及文件

- `os/src/fs/files/epoll/registry.rs`
- `os/src/syscall/io_mpx/epoll.rs`

## 验证

- `make TARGET_ARCH=riscv64`：通过，完成 RISC-V 与 LoongArch64 release 构建。
- `make TARGET_ARCH=riscv64 log`：通过。
- 直接运行 `cargo check --manifest-path os/Cargo.toml --target riscv64gc-unknown-none-elf` 时，宿主锁定的 nightly 与缓存 `printf-compat` 的 `VaList` API 不兼容而失败；错误来自第三方依赖，未触及本轮 epoll 代码，项目标准 `make`/`make log` 入口仍通过。
- 修复后的 RISC-V 短时 BuildStorm 已完成 `BUILDSTORM_TOOLCHAIN`、`BUILDSTORM_MINIBUILD` 并进入 `pre-build tg-xtask`；测试窗口尚未再次执行正式 runtime，因此不能据此宣称原 `Bad file descriptor` 已运行时消除，也不宣称完整 BuildStorm、`BUILDSTORM_COMPILE` 成功或端到端耗时通过。
- 随后的 QEMU 复跑因 `scripts/riscv64.mk` 保留 guest 文件系统状态，在 initproc 创建已存在的 `/proc/1` 时得到 `EISDIR`，未进入 BuildStorm；该启动环境问题与 epoll 修复无关。
- `git diff --check`：通过。全量 `cargo fmt --check` 受工作区其他既有差异影响，未用于验收本轮修改。
