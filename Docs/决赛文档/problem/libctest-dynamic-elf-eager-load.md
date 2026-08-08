# libctest 动态 ELF 启动失败

## 背景

`log.ans` 使用 RISC-V pre-test 镜像运行 `libctest-musl`。同一组测试中，
`entry-static.exe` 全部通过，而 `entry-dynamic.exe` 的所有测试均以 `status 255`
结束。

## 现象

日志没有 panic，但动态测试从第一个用例开始全部失败；静态测试的 217 个
用例均输出 `Pass!`。带 debug 日志复现时，动态程序完成 `execve` 后只执行到
动态链接器的初始化 syscall，随后直接 `exit_group(-1)`。

## 分析

最近的 ELF loader 优化把主程序和动态解释器的对齐 `PT_LOAD` 段改成了
file-backed lazy VMA。动态链接器启动时需要读取并修改重定位相关状态，当前
文件页 fault 路径不能可靠覆盖这条启动路径，导致解释器在完成少量初始化后退出。

## 根因

动态 ELF 的 `PT_LOAD` 段使用了与普通 `mmap` 相同的延迟文件页映射语义，而
原有 ELF loader 使用 eager framed 映射。该路径改变了动态解释器启动的内存
可见性和写时复制行为，造成所有动态 libc 测试的共同失败。

## 修复

恢复 ELF 主程序和动态解释器 `PT_LOAD` 段的 eager framed 映射：从已打开的
可执行文件读取段内容，建立物理页并保留 ELF 段的权限和 BSS 零填充语义；普通
用户 `mmap` 的懒加载路径不变。

涉及文件：

- `os/src/mm/memory_set/elf_loader.rs`

## 验证

- `make TARGET_ARCH=riscv64`：通过。
- `make TARGET_ARCH=loongarch64`：通过。
- `timeout 60s make run TARGET_ARCH=riscv64`：运行至 `shutdown!`，libctest
  动态用例不再出现 `FAIL`，共输出 217 个 `Pass!`。
