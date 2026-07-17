# LTP fs_bind24 子目录 bind 的 shared-slave 传播修复

## 背景

LTP `fs_bind24.sh` 覆盖 shared mount 内部子目录的 bind、递归 slave/shared 状态转换，以及后续在 bind target 下创建 mount 的反向传播。当前 VFS 仍以路径化 `MountTable` 和目录镜像提供可观察的 bind mount 视图，因此传播表必须同时保存拓扑状态与各 bind root 的路径对应关系。

## 现象

`log.ans` 的 musl 和 glibc 均在最后一个 `mount --bind dir4 dir2/fs_bind_check` 后失败：`dir4` 有文件 `ls`，但预期的 `dir1/1/2/fs_bind_check` 为空。每段 Summary 都是 `passed 14 failed 2 broken 0`，没有 panic 或 TBROK。

## 分析

脚本先将 `dir1` bind 到自身并标记为 `rshared`，然后将其内部目录 `dir1/1/2` bind 到 `dir2`。之后 `dir1` 先变为 rslave，再变为 rshared，最后再次变为 rslave。新 mount event 在 `dir2/fs_bind_check` 创建时，应沿 `dir2` 的 shared group 进入 `dir1` 的 slave branch，并保留 `dir1/1/2` 这个 bind source 子目录偏移。

现有实现存在三个相互叠加的问题：

- bind source 的 unbindable 校验使用覆盖 source 的顶层 mount，但状态复制只查找精确 mountpoint；内部目录 `dir1/1/2` 因此不会向 `dir2` 继承 propagation state。
- 已经是 shared-slave 的 `dir1` 再执行 `--make-rslave` 时，代码以其自身 shared group 覆盖原有 `master_group`，切断了来自上游 group 的事件。
- propagation target 仅将 event 相对路径附加到 receiver root；对 `dir2` 这种由 `dir1/1/2` bind 得到的根，错误生成 `dir1/fs_bind_check`，丢失了 `1/2` 偏移。

## 根因

`MountTable` 对 bind source 的可见层选择、shared-slave 状态转换和子目录 bind 的 peer 路径映射不一致。前两项造成传播链不可达，后一项即使链路可达也会将目录镜像落到错误路径。

## 修复

修改 `os/src/fs/mount.rs`：

- bind source 使用 `top_mount_index_for_path()`，从覆盖内部 source 目录的可见顶层 mount 继承 shared/master state，与 unbindable 校验一致。
- `MS_SLAVE` 转换优先保留既有 `master_group`；仅在不存在上游 master 时才使用刚退出的 shared group。
- 计算 receiver target 时，若 event 发生在 bind mount 下且 receiver 覆盖该 bind source，则拼接 bind source 相对 receiver root 的偏移，再附加 event 相对路径。

这保持 syscall 层和目录镜像层不变，只修正挂载表的传播语义。

## 涉及文件

- `os/src/fs/mount.rs`
- `Docs/决赛文档/problem/fs-bind24-subtree-shared-slave-propagation.md`
- `Docs/决赛文档/problem/README.md`
- `Docs/决赛文档/开发日志.md`
- `Docs/决赛文档/ai.log`
- `Docs/决赛文档/AI_INTERACTION.md`

## 验证

- `rustfmt --edition 2021 --check os/src/fs/mount.rs` 与 `git diff --check` 通过。
- `make TARGET_ARCH=riscv64` 完成 RISC-V 与 LoongArch64 release 构建；仅有既有 vendored `smoltcp` warnings。
- `timeout 180s make run TARGET_ARCH=riscv64` 在 RISC-V QEMU 中完成维护者的单测入口。musl 与 glibc `fs_bind24` 均为 `passed 15 failed 0 broken 0 skipped 0 warnings 0`，最后的 propagation check 与全部 umount 均为 `TPASS`，正常 `shutdown!`。
- LoongArch64 未运行此 LTP 单测。
