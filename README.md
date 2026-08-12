# Ya2yOS

![alt text](Docs/img/华南理工大学.png)
项目成员：饶晓杰

本项目主要基于2025年塔特林设计局的参赛作品[TatlinOS](https://gitlab.eduxiji.net/educg-group-36002-2710490/T202510487995221-883)的2025-tatlin分支。

截至8.12本项目的得分情况如表所示：

|测试点     | glibc-la | glibc-rv | 总分  |
| :---:| --- | ---| ---|
|buildstorm |  20.0    | 124.7   | 144.7 |
|cagent     |  199.1   | 199.1   | 398.2 |
|总分       |  219.1   | 323.8   | 542.9 |

## 构建方法

### Docker环境配置

首先前往[Docker官网](https://www.docker.com/)安装Docker。然后在终端输入
```bash
docker pull zhouzhouyi/os-contest:20260510
```
下载Docker镜像。

### 构建项目
在项目根目录输入
```bash
make docker
```
进入Docker环境。然后输入
```bash
make
```
默认进行双架构的编译。你也可以根据项目根目录的`Makefile` 选择性构建。

## 项目结构说明

Ya2yOS 采用“内核、用户态程序、可复用 crate、构建脚本、文档”分层组织。仓库的主要目录如下：

```text
.
├── os/                  # 内核 crate：启动、任务、内存、文件系统、网络、设备和 syscall 实现
│   ├── src/             # 内核源码
│   │   ├── arch/        # RISC-V64、LoongArch64 架构相关代码、中断和上下文切换
│   │   ├── drivers/     # 块设备、VirtIO、网卡等设备驱动
│   │   ├── fs/          # VFS、ext4、页缓存、挂载和动态链接相关文件系统功能
│   │   ├── mm/          # 地址空间、页表、物理页帧、堆、共享内存和用户空间访问
│   │   ├── net/         # TCP/UDP/Unix socket、路由和网络设备抽象
│   │   ├── signal/      # 信号类型、投递、处理器和定时器
│   │   ├── syscall/     # Linux 兼容系统调用及其 fs/mm/net/task 等子模块
│   │   ├── task/        # 进程、线程、调度器、futex、clone 和任务管理
│   │   ├── timer/       # 时钟、定时器、时间结构和资源使用统计
│   │   ├── trap/        # 异常、中断和陷阱处理
│   │   └── utils/       # 内核通用工具、性能统计和辅助数据结构
│   ├── build.rs         # 内核构建阶段的辅助逻辑
│   └── Cargo.toml       # 内核 crate 配置
├── user/                # 用户态运行库和测试/基准程序
│   ├── src/lib.rs       # no_std 用户库、启动代码和常用系统调用封装
│   ├── src/syscall/     # 用户态系统调用入口及 socket 等接口
│   ├── src/arch/        # 用户态架构适配和链接脚本
│   └── src/bin/         # initproc、LTP、cagent、网络测试和性能测试程序
├── crates/              # 内核和用户态共同使用的第三方或本地依赖
│   ├── buddy_system_allocator/ # 伙伴堆分配器
│   ├── lwext4_rust/     # ext4 文件系统 Rust 封装
│   ├── smoltcp/         # TCP/IP 协议栈
│   └── cty/             # C 类型定义兼容 crate
├── scripts/              # 架构、用户程序和测试相关的 Makefile 片段/脚本
│   ├── riscv64.mk       # RISC-V64 构建参数
│   ├── loongarch64.mk   # LoongArch64 构建参数
│   ├── user.mk          # 用户程序构建和打包规则
│   └── *_testcode.sh    # buildstorm、cagent 等测试辅助脚本
├── Docs/                 # 设计文档、开发日志、问题复盘、AI 修改记录和图片
├── 2026_testsuits_img/   # 评测镜像及相关测试套件
├── Makefile              # 顶层构建、运行、调试、性能统计和文档生成入口
└── README.md             # 项目说明、构建方法和参考资料
```

### 代码阅读入口

- **启动与总控**：从 [`os/src/main.rs`](./os/src/main.rs) 进入内核初始化流程；架构相关实现位于 [`os/src/arch/`](./os/src/arch/)。
- **系统调用**：系统调用分发和具体实现位于 [`os/src/syscall/`](./os/src/syscall/)，用户态封装位于 [`user/src/syscall/`](./user/src/syscall/)。
- **任务与调度**：进程、线程和调度相关代码位于 [`os/src/task/`](./os/src/task/)。
- **内存管理**：地址空间、页表和内存映射相关代码位于 [`os/src/mm/`](./os/src/mm/)。
- **文件与网络**：分别从 [`os/src/fs/`](./os/src/fs/) 和 [`os/src/net/`](./os/src/net/) 开始阅读。
- **用户程序**：各个可执行程序的入口位于 [`user/src/bin/`](./user/src/bin/)，其中 `initproc` 负责用户态初始进程。

项目文档和开发日志均位于 [Docs](./Docs/) 目录下。

整个内核详细的设计文档请参考[内核设计文档](./Docs/ya2yos/typst/main.typ)；在
仓库根目录中执行 `typst compile --root . Docs/ya2yos/typst/main.typ Docs/ya2yos/typst/ya2yos-kernel-design.pdf` 
可生成 PDF。关于文档，更详细的构建信息参考[Doc](Docs/ya2yos/README.md)。

在引入 skills 后，AI 的每一次修改均会在 [Docs/初赛文档/AI_INTERACTION.md](./Docs/决赛文档/AI_INTERACTION.md) 
和 [Docs/初赛文档/ai.log](./Docs/决赛文档/ai.log) 这两个文件中记录。前者注重人机交互过程，后者注重 AI 的修改范围。
与 AI 的主要交互方式是让 AI 通过输出日志进行修改，本人只做最后的原因分析和验收。
[bug 修复文档](./Docs/决赛文档/problem/)
文档中的图均在 [Docs/img/](./Docs/img/) 目录下。
[演示视频(TODO)](https://1839796361.share.123pan.cn/123pan/ihR2Td-F5MpH?pwd=ya2y#)

主要开发分支在 `nightly`，`main` 分支只记录可以在评测机正常跑分的版本。

## 参考项目及书籍:

+ [rCore-Tutorial-Book-v3](https://rcore-os.cn/rCore-Tutorial-Book-v3/index.html)
+ [xv6-book](https://pdos.csail.mit.edu/6.1810/2025/xv6.html)
+ [TatlinOS](https://gitlab.eduxiji.net/educg-group-36002-2710490/T202510487995221-883)
+ [RocketOS](https://gitlab.eduxiji.net/educg-group-36002-2710490/T202510213995926-2475)
+ [Chronix](https://gitlab.eduxiji.net/educg-group-36002-2710490/T202518123995568-675)
+ [StarryOS](https://github.com/rcore-os/tgoskits)
+ [Linux](https://elixir.bootlin.com/linux/v7.0/source)
+ [OSTEP](https://pages.cs.wisc.edu/~remzi/OSTEP/)
+ LinuxUNIX系统编程手册
+ UNIX网络编程 卷1：套接字联网API（第三版）
+ UNIX环境高级编程（第三版）
