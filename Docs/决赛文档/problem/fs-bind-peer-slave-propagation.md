# LTP fs_bind peer/slave 传播与同树 bind panic

## 背景

路径化 VFS 以 `MountTable` 记录挂载层，并在 bind 时镜像目录树，使普通路径查找能够观察到传播结果。维护者将 `fs_bind01` 至 `fs_bind24` 加入 RISC-V musl/glibc LTP 入口后，新的 `log.ans` 暴露了 shared 子树、级联 slave 和同树 bind 三类问题。

## 现象

原始日志中，`fs_bind17` 至 `fs_bind20` 在已建立 shared 关系的父挂载下创建子挂载后，peer 路径缺少对应副本；`fs_bind21` 的 `dir1 -> dir2 -> dir3 -> dir4` slave 链也没有继续传播。`fs_bind22` 执行 `mount --bind parent parent/child2` 后，在内核态递归镜像目录树，最终触发 `StorePageFault` panic。

## 分析

旧挂载表只有 `shared_group`，普通挂载只向同组 peer 展开一层副本：

- `--make-rslave` 会直接清除 shared 状态，没有保存其 master peer group；因此 slave 既不能接收来自 master 的事件，也不能把事件继续传递到其后代。
- 同一 mount event 创建的 peer 子挂载在后续 `--make-rshared` 时没有被编入同一新 group，导致 `parent1/child1` 下的新事件不能传到 `parent2/child1` 和 `share1/child1`。
- `mirror_bind_tree(parent, parent/child2)` 在遍历源目录时重新看到刚创建的目标分支，继续递归进入 `parent/child2/child2/...`，耗尽内核栈。

## 根因

挂载传播状态缺少 Linux mount propagation 所需的单向 master 关系，且事件展开算法没有沿 shared slave 链递归。目录镜像层则把 bind mount 的 mount-root 语义错误实现为普通目录递归复制，未识别目标位于源目录树内的环。

## 修复

- `MountEntry` 新增 `master_group`。`MS_SLAVE` 将转换前的 shared group 保存为 master，`MS_SHARED` 允许 slave 保留 master 的同时获得自己的 peer group，private/unbindable 清除全部传播关系。
- 新增从目标父挂载递归展开 shared peer 和 `master_group` slave 后代的传播目标计算；每个副本继承接收父挂载的传播状态，保证 slave 分支只向下游转发、不回流到 master。
- `--make-rshared` 会把同一 mount event 的 peer 副本放入同一个新 group，修复 shared 子挂载的后续传播。
- 目录镜像检测 target 位于 source 的子树时跳过该源分支，消除 `fs_bind22` 的无限递归和 panic。

## 验证

- `cargo fmt --manifest-path os/Cargo.toml -- --check` 与 `git diff --check` 通过。
- 根目录 `make` 完成 RISC-V 和 LoongArch64 release 构建，仅有既有 vendored `smoltcp` warnings。
- RISC-V `make log TARGET_ARCH=riscv64` 完成 debug 构建；debug QEMU 在 300 秒上限前完成 `fs_bind01` 至 `fs_bind12`，各 Summary 均为 `failed 0`。
- RISC-V release QEMU 运行当前 fs_bind 列表：`fs_bind17` 至 `fs_bind21` 均为 `failed 0`，其中 `fs_bind21` 为 `passed 33 failed 0 broken 0`，且未见内核 panic。
- `fs_bind22` 不再 panic；首次 `fs_bind_check parent parent/child2` 仍因路径化 VFS 没有独立 mount-root dentry 而失败。真实 Linux bind 后两端通过同一 mount root 暴露同 inode，当前物理目录镜像无法无环地构造该视图。后续挂载、卸载和子目录传播检查均 TPASS。完整支持此场景需要 VFS 层引入独立 mount-root 视图，而不是继续扩展 syscall 层目录复制。
