# execv01: MAP_SHARED 文件映射未跨 exec 共享

## 背景

LTP `execv01` 会在父测试进程中创建共享结果页，`SAFE_FORK()` 后让子进程 `execv(execv01_child)`。子程序启动后调用 `tst_reinit()`，通过环境变量 `LTP_IPC_PATH` 重新打开同一个结果文件，并用 `mmap(MAP_SHARED)` 把 LTP 结果计数映射回来。

这条路径要求不同进程、不同 `mmap()` 调用只要映射同一个文件页，就能通过 `MAP_SHARED` 看到同一份结果计数。

## 现象

`log.ans` 中 musl 和 glibc `execv01` 都能看到子程序输出 `TPASS`：

```text
execv01_child.c:23: TPASS: ./execv01_child executed
tst_test.c:1449: TBROK: Test haven't reported results!
Summary:
passed   0
failed   0
broken   1
```

外层退出码为 512。说明 `execv()` 本身成功，失败发生在父测试框架回收子进程后读取共享结果计数时。

## 分析

LTP `tst_test.c` 在 `run_tests()` 中先保存 `results` 计数，执行 `test_all()` 后调用 `tst_reap_children()`，再比较共享计数。如果计数未变化，就报：

```text
Test haven't reported results!
```

`execv01_child` 已经调用 `tst_res(TPASS, ...)` 并打印 `TPASS`，但父进程的 `results->passed` 没变，说明子进程写入的是另一份映射页。

Ya2yOS 原有 `MAP_SHARED` 实现用 `GROUP_SHARE` 以 `MapArea.groupid` 为单位共享物理帧。这个机制覆盖了 `fork()` 继承同一个 `MapArea` 的场景，但 `execv01_child` 在 `execve()` 后调用 `tst_reinit()`，会重新 `open + mmap(MAP_SHARED)` 同一个结果文件，得到新的 `MapArea` 和新的 `groupid`。因此子进程重新映射后分配的是另一页物理内存，写入不会被父进程已有映射看到。

## 根因

`MAP_SHARED` 文件映射只按 fork 继承的 `groupid` 共享，没有按文件页共享。不同 `mmap(MAP_SHARED, same file, same offset)` 调用之间缺少共同的物理页缓存，导致 LTP exec 后重新初始化的共享结果页与父进程结果页分裂。

## 修复

在 `GROUP_SHARE` 中增加文件页级共享缓存：

- key 使用文件路径和文件页下标 `(path, page_index)`；
- `mmap_write_page_fault()` 对 `MAP_SHARED` 文件映射先查询文件页缓存；
- 命中缓存时直接把已有 `FrameTracker` 映射到当前进程页表；
- 未命中时按原逻辑分配物理页、从文件读入内容，并把该页登记到文件页缓存。

这样 `fork` 继承的共享映射仍可继续使用原有 `groupid` 路径，`execve()` 后重新打开同一文件再 `mmap(MAP_SHARED)` 的路径也能复用同一物理页。

涉及文件：

- `os/src/mm/group.rs`
- `os/src/mm/page_fault_handler.rs`

## 验证

已执行：

```text
make
timeout 120s make run
```

当前默认 `TARGET_ARCH=loongarch64`，`make` 通过。

在 `initproc` 指向 `execv01` 的复现配置下，单跑 musl/glibc `execv01` 后结果为：

```text
execv01_child.c:23: TPASS: ./execv01_child executed

Summary:
passed   1
failed   0
broken   0
skipped  0
warnings 0
```

glibc 单测同样输出 `passed 1 failed 0 broken 0`，`RESULT GLIBC LTP SINGLE CASE execv01 : 0`。未再出现 `tst_test.c:1449: TBROK: Test haven't reported results!`。
