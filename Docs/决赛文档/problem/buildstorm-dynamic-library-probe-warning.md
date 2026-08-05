# BuildStorm 原生动态库探测误报内核 WARN

## 背景

final-2026 BuildStorm 使用 Debian 原生动态链接器、LLVM 19、Rust toolchain 和多组并行
`rustc` 进程。动态链接器会沿 `RPATH`、`RUNPATH` 和系统库目录依次尝试多个 `.so`
路径；某次尝试没有命中兼容映射或文件时，需要保留原路径和真实 `open(2)` 结果，让
动态链接器继续搜索。

## 现象

`server.ans` 在正式 BuildStorm 编译期间出现 35 条内核 WARN，均来自同一个位置：

```text
[WARN] ... Warning: map_dynamic_link_file cannot find DL path for path:/usr/lib/libatomic.so.1
[WARN] ... Warning: map_dynamic_link_file cannot find DL path for path:/usr/lib/llvm-19/lib/libclang.so
[WARN] ... Warning: map_dynamic_link_file cannot find DL path for path:/lib/lib/libffi.so.8
```

涉及 `libatomic`、`libstdc++`、`libz3`、`libz`、`libzstd`、LLVM/Clang 和 `libffi` 的
原生搜索候选。WARN 后 BuildStorm 继续编译，说明这些记录不是缺库导致的立即失败。

同一日志中还有两类用户态 Cargo/Rust warning，与本问题不同：

- 0 字节 `dep-graph.bin` 是旧 rename 失败遗留的增量缓存，Cargo 正在丢弃并重建；
- `set_fdt_addr_phys_if_valid` 的 `dead_code` warning 来自只读 BuildStorm 测试源码。

## 分析

`sys_openat()` 在实际打开文件前调用 `map_dynamic_link_file()`。这个兼容函数先检查请求
是否需要按 basename 重定向到旧 `/glibc/lib` 或 `/musl/lib`；找不到兼容目标时返回
原路径。随后通用 `open()` 按原路径完成正常查找，并把成功或 `ENOENT` 返回给用户态。

因此，“没有 legacy 兼容映射”只是路径映射查询的 `None` 分支，不代表文件不存在，
也不代表 exec 或动态链接失败。对 native loader 而言，候选路径返回 `ENOENT` 后继续
搜索同样是标准控制流。旧代码却在返回原路径前调用 `warn!()`，把正常探测提升成了内核
异常，并在多个并行 rustc 进程中重复输出。

## 根因

动态库兼容层混淆了“没有历史镜像重定向规则”和“动态库加载失败”：前者是正常、可恢复
的查询结果，却被无条件记录为 WARN。路径选择和错误传播本身没有失败。

## 修复

`map_dynamic_link_file()` 在没有 legacy 映射时继续返回原路径，并把该记录降为 debug：

- 不新增库白名单或伪造重定向；
- 不改变 native/legacy 动态库选择；
- 不吞掉 `open(2)` 的真实错误；
- warning 级别运行不再为动态链接器的正常搜索输出内核 WARN。

`map_dynamic_link_file_directly_map()` 的 warning 保留。它只在 ELF 声明的解释器原路径已经
打开失败、同时也没有兼容解释器映射时触发，属于真正会使 `execve` 失败的异常路径。

## 涉及文件

- `os/src/fs/map_dynamic_link.rs`

## 验证

- `server.ans` 统计到 35 条目标 WARN，确认全部来自
  `map_dynamic_link_file()` 的同一 `warn!()` 分支。
- `rustfmt --edition 2021 --check os/src/fs/map_dynamic_link.rs`：通过。
- `make build-arch TARGET_ARCH=riscv64`：RISC-V release 构建通过；
- `make build-arch TARGET_ARCH=loongarch64`：LoongArch64 release 构建通过；两次构建仅有既有
  Cargo config 和 customized `smoltcp` warning。
- 未重跑完整 BuildStorm。分析期间维护者已有的 QEMU/GDB 会话仍使用修改前内核，不能
  作为修复后运行时验证；本次未终止或复用该会话。
