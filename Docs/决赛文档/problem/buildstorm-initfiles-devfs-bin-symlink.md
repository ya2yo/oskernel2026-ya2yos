# BuildStorm 根文件系统 initfiles、/dev/null 与 /bin 符号链接兼容

## 背景

决赛 BuildStorm 镜像采用 Debian 风格根文件系统，`/bin` 是指向 `/usr/bin` 的符号链接，
工具链启动和脚本重定向依赖可用的 `/dev/null`。这与原先面向 BusyBox 镜像的启动期
wrapper 注入假设不同。

## 现象

原始 `log.ans` 没有 `TFAIL`、`TBROK`、panic 或测试 Summary；BuildStorm 在早期启动路径
被外部终止。定向 RISC-V 日志显示初始化中 `create_dir("/proc")` 失败，随后 DevFS 文件
没有完成创建，访问 `/dev/null` 会出现 `ext4_fopen: /dev/null, rc = 2`。

此外，旧逻辑无条件在 `/bin/*` 写 BusyBox 链接和 wrapper。当 `/bin -> /usr/bin` 时，写入
会透过符号链接覆盖真实 `/usr/bin/*`，使 Debian 用户空间工具被错误替换。

## 分析与根因

`create_dir()` 将“查询已存在目录”和“创建目录”混为一次带 `O_CREAT | O_RDWR |
O_DIRECTORY` 的打开。对已存在的目录，该组合返回 `EISDIR`；`create_init_files()` 因此
在 DevFS 注册前返回。`fs::init()` 又吞掉了这个错误，日志缺少能直接定位初始化失败的信号。

原 wrapper 逻辑则假定 `/bin` 是普通目录，未识别 Debian 的 merged-/usr 布局。

## 修复

- `create_dir()` 先以只读目录方式查询；仅当结果为 `ENOENT` 时再创建目录。
- `fs::init()` 对 `create_init_files()` 失败显式记录错误，不再静默忽略。
- 检查 `/bin` inode 类型；它是符号链接时跳过 BusyBox wrapper 注入，保留真实
  `/usr/bin` 内容。

修改文件：

- `os/src/fs/kernel_fs_ops/initfiles.rs`
- `os/src/fs/mod.rs`

## 验证

本次主补丁阶段的 RISC-V debug 启动日志已出现 `create_init_files success!`，且不再出现
`ext4_fopen: /dev/null, rc = 2`。最终整理后，RISC-V 与 LoongArch64 release 均重新构建
通过。

完整 BuildStorm 编译/性能测例没有在本轮完成；这些证据仅验证根文件系统初始化和工具链
启动前置条件，不等同于整个决赛测例通过。
