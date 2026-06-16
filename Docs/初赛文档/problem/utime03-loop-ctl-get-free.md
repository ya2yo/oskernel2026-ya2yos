# LTP utime03: LOOP_CTL_GET_FREE 返回语义

## 背景

`utime03` 在准备测试文件系统环境时会通过 `/dev/loop-control` 获取空闲 loop 设备，然后继续 mount 临时测试目录。当前单跑配置为 LoongArch `ltp-musl` 的 `utime03`。

## 现象

`log.ans` 中 `utime03` 没有进入真正的 `utime()` 断言，而是在设备准备阶段中断：

```text
[sys_ioctl] fd=3, cmd=19586, arg=32770
not find area
[syscall ret --- Err] Ioctl ret = Bad address
tst_device.c:100: TINFO: Couldn't find free loop device
tst_device.c:354: TBROK: Failed to acquire device
```

外层 initproc 随后打印 `FAIL LTP CASE utime03 : 512`，LTP Summary 为 `broken 1`。

## 分析

`fd=3` 对应 `/dev/loop-control`，`cmd=19586` 对应 `LOOP_CTL_GET_FREE`。原实现找到空闲 loop 号后调用 `copy_to_user(memory_set, arg, &nr)`，把 ioctl 第三个参数当作用户态输出指针。

但 `LOOP_CTL_GET_FREE` 的 Linux 语义是直接通过 ioctl 返回值返回空闲 loop 号，第三参数不是输出地址。LTP 传入的 `arg=32770` 不是有效用户指针，因此地址检查失败并返回 `EFAULT`。

## 根因

`DevLoopControl::ioctl(LOOP_CTL_GET_FREE)` 错把返回值语义实现成了用户指针写回语义，导致 LTP 获取 loop 设备时收到 `EFAULT`，进而 `TBROK`。

## 修复

修改 `os/src/fs/files/loopdev.rs`：

- `LOOP_CTL_GET_FREE` 找到空闲 loop 号后直接 `Ok(nr as usize)`；
- 不再对 ioctl 第三个参数执行 `copy_to_user`；
- 将该实现中的 `memory_set` 参数标记为 `_memory_set`，避免未使用参数警告。

## 验证

已执行：

```text
make
timeout 120s make run
```

验证结果：

```text
tst_device.c:96: TINFO: Found free device 0 '/dev/loop0'
utime03.c:74: TPASS: utime(TEMP_FILE, NULL) passed
Summary:
passed   1
failed   0
broken   0
skipped  0
warnings 0
```

外层仍会打印 `FAIL LTP CASE utime03 : 0`，这是当前 initproc 包装器的固定前缀加退出码 0，不代表 LTP 失败。
