# access02 修复过程

## 当前问题

```bash
[WARN] [HART0] [PID 4] [TID 4] [kernel] hart 0 Exception(LoadPageFault) in application, bad addr = 0xffffffffffffff68, bad instruction = 0x150005db0c, kernel killed it.
[WARN] [HART0] [PID 4] [TID 4] don't send SIGSEGV, just exit the process
[WARN] [HART0] [PID 3] [TID 3] [kernel] hart 0 Exception(StorePageFault) in application, bad addr = 0x15000a5760, bad instruction = 0x150004eae0, kernel killed it.
[WARN] [HART0] [PID 3] [TID 3] don't send SIGSEGV, just exit the process
```

## 第一处 LoadPageFault

bad addr 刚好是 -152 可能是在创建子进程的时候忘记对trap的位置继续初始化，检查clone的实现，发现child刚创建的时候确实 `trap_cx_ppn=0,trap_cx_bottom=0`,而且后续也没有进行修改。
修复方法：
`*child_inner.trap_cx() = *parent_inner.trap_cx();` 在fork分支增加映射父进程trap_cx。

## 第二处 StorePageFault

因为4是3的子进程，根据先前的经验判断，3尝试写入的地址应该是被4释放了，导致 `StoragePageFault`。
修改方式是在进程退出时先判断memory_set的引用次数，次数为一才回收页

## 新的问题

```bash
ccess02.c:129: TFAIL: execute file_x as root failed: ENOENT (2)
access02.c:129: TFAIL: execute file_x as nobody failed: ENOENT (2)
access02.c:60: TFAIL: access(symlink_f, F_OK) as root failed: ENOENT (2)
access02.c:60: TFAIL: access(symlink_f, F_OK) as nobody failed: ENOENT (2)
access02.c:60: TFAIL: access(symlink_r, R_OK) as root failed: ENOENT (2)
access02.c:60: TFAIL: access(symlink_r, R_OK) as nobody failed: ENOENT (2)
access02.c:60: TFAIL: access(symlink_w, W_OK) as root failed: ENOENT (2)
access02.c:60: TFAIL: access(symlink_w, W_OK) as nobody failed: ENOENT (2)
access02.c:60: TFAIL: access(symlink_x, X_OK) as root failed: ENOENT (2)
access02.c:60: TFAIL: access(symlink_x, X_OK) as nobody failed: ENOENT (2)
```

根据ChatGPT没找到一方面是没有/bin/sh的符号链接，另一方面是符号链接处理的有问题,原来的处理逻辑使用读到的路径，忽视了可能是相对路径。

修改如下：

`os/src/fs/kernel_fs_ops/initfiles.rs`: 添加 `"/bin/sh"` 指向 busybox
`os/src/fs/ext4_lw/inode.rs`:

```rust
let next_path = if file_path.starts_with('/') {
    // 绝对路径 symlink
    file_path.to_string()
} else {
    // 相对路径 symlink
    join_path(path, file_path)
};
```

```rust
/// 路径规范函数
fn join_path(base: &str, rel: &str) -> String {
    let mut comps = Vec::new();

    for part in base.split('/') {
        if !part.is_empty() {
            comps.push(part);
        }
    }

    // 去掉当前文件名
    comps.pop();

    for part in rel.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                comps.pop();
            }
            x => comps.push(x),
        }
    }

    format!("/{}", comps.join("/"))
}
```
