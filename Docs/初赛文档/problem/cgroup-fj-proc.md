# 单独运行cgroup_fj_proc 卡死

运行cgroup_fj相关测试时，由于 `FAIL LTP CASE cgroup_fj_function.sh : 2`等相关测试的失败导致cgroup_fj_proc会直接卡死，没有信号告诉这个任务退出。通过显示地调用`./cgroup_fj_function.sh cpuset` 来避免这个错误。

修改后运行panic
单独运行时最后panic`[kernel] Panicked at src/arch/riscv64/qemu/page_table.rs:293 ly: a valid pte without COW flag found at 0x0` 经过检查，发现原来的设计中是这样的
> // 在原来的写法中，不论是否有cow标志都会返回true，这里改为没有cow标志时返回false
> // 简单测试发现此处valid的pte都具有cow标志，在这放一个panic，看看未来是否会出现panic
因此此处我将其改回return false;
