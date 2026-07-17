# LTP fs_bind_move05 私有子树移入 shared 父挂载后的传播

## 背景

LTP `fs_bind_move05.sh` 覆盖一个 private bind mount 被 `mount --move` 移入 shared 父挂载的场景。移动后，在该子树内部创建的新 bind mount 必须传播到 shared peer 的相同相对路径；原始 source 与先前独立 bind 的路径不得获得该传播。

当前 Ya2yOS 以 `MountTable` 保存传播关系，并在 syscall 层镜像目录树，使路径化 VFS 可以观察 bind mount 的目录视图。

## 现象

`log.ans` 中的 RISC-V musl `fs_bind_move05` 在以下操作后失败：

```sh
mount --move dir parent2/child2
mount --bind disk1 parent2/child2/grandchild
```

`parent2/child2/grandchild` 未传播至 `share2/child2/grandchild`；随后的 `disk2` bind 也未回传，`umount parent2/child2/grandchild/a` 返回 `EINVAL`，cleanup 报告残留挂载并最终超时。

## 分析

`move_mount()` 已能将 source subtree 重定位，并在目标父挂载的 peer 下克隆 subtree。但原移入 `parent2/child2` 的根条目仍保留 `dir` 的 private propagation state，后续位于该根下的 bind event 因而不会启动 shared 传播。

即使为移动根赋予 shared group，旧的通用 target 计算也只会将 event 的 `grandchild` 相对路径追加到 `share2`，遗漏移动根本身在 peer 中的 `child2` 偏移。已为一次 move 创建的 peer 根共享同一个 `event_group`，该信息可用于在传播时先匹配对应 moved root，再保留完整相对路径。

## 根因

`MS_MOVE` 的路径化挂载模型缺少两项语义：

- 移动根没有从各自目标接收父挂载继承 shared/master/unbindable propagation state。
- 移动根内部的新事件没有先映射到同一 move event 的 peer root，因此丢失目标父挂载下的子目录偏移。

这使 private subtree 被移入 shared parent 后的后续挂载事件既不能正确启动，也不能落到 peer 的对应路径。

## 修复

修改 `os/src/fs/mount.rs`：

- `move_mount()` 在修改原移动根和创建每个 peer 副本前，快照对应目标接收端的 propagation state；只对 subtree 根应用该状态，子挂载继续保留自身属性。
- `propagation_targets()` 对 shared mount 先枚举同一 `event_group` 的可见 peer root，并用当前 event 相对该根的路径生成目标。之后仍保留既有 shared/slave 图遍历。

实现限定在挂载表；没有修改 syscall ABI、目录镜像逻辑、只读 LTP 测试源码，亦未改动维护者的单测入口。

## 涉及文件

- `os/src/fs/mount.rs`
- `Docs/决赛文档/problem/fs-bind-move05-private-shared-propagation.md`
- `Docs/决赛文档/problem/README.md`
- `Docs/决赛文档/开发日志.md`
- `Docs/决赛文档/ai.log`
- `Docs/决赛文档/AI_INTERACTION.md`

## 验证

- `make TARGET_ARCH=riscv64` 完成 RISC-V 和 LoongArch64 release 构建；只有既有 vendored `smoltcp` warnings。
- `timeout 180s make run TARGET_ARCH=riscv64` 在 RISC-V QEMU 中运行维护者现有的 musl/glibc `fs_bind_move05` 单测入口；两者均为 `passed 27 failed 0 broken 0 skipped 0 warnings 0`，全部 propagation 与 `umount` 断言为 `TPASS`，最终正常 `shutdown!`。
- LoongArch64 未运行该 LTP 单测。
