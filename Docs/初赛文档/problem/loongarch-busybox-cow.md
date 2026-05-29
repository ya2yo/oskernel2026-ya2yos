# 龙芯架构 busy_box 测试失败

根据cursr分析：
已分析并修复新的 log.ans 末尾 LoadPageFault。

根因是 LoongArch 的 COW 页表标志处理不完整：fork 后私有 mmap/brk 页进入 COW 时只清了 WRITEABLE，但没有清 DIRTY；解除 COW 后也没有补回 DIRTY 并刷新 TLB。结果父子进程的私有堆/mmap 页写入隔离不可靠，busybox shell 在后续 wait4/SIGCHLD 返回后读到了被污染的 malloc/指针状态，最终访问 0x2a2343e... 触发 LoadPageFault。

修改点：

os/src/trap/mod.rs：LoongArch PageModifyFault 先尝试走 COW handler，非 COW 页才按原逻辑置 dirty。
os/src/arch/loongarch64/qemu/page_table.rs：进入 COW 时清 WRITEABLE | DIRTY；解除 COW 时恢复 WRITEABLE | DIRTY 并刷新 TLB；非 COW 情况返回 false，不再 panic。
验证：

make loongarch64-build 通过。
make run 后日志中 LoadPageFault count = 0、StorePageFault count = 0。
busybox 文件操作尾段已跑到 #### OS COMP TEST GROUP END busybox-musl ####，末尾正常 shutdown!。
