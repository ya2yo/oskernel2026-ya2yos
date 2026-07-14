= 网络与 I/O 设备

== 网络栈

网络模块以 smoltcp 为协议栈基础 #cite(<smoltcp>)，围绕 `SocketSet`、监听表、路由器和网络服务循环组织。`net::init_network` 接收设备容器并建立 loopback 与可用以太网设备；套接字实现被包装为 VFS `File`，故 `socket` 创建的 fd 可以进入 read/write/poll 等通用路径。

当前实现覆盖 TCP、UDP 和 Unix domain socket，并提供 bind、listen、accept、connect、send/recv、地址查询、控制消息和部分 socket option 的系统调用编排。Unix socket 的接收队列有字节上限；阻塞/非阻塞发送根据队列容量等待或返回 `EAGAIN`，防止无界消息积压耗尽内核堆。

== 数据收发路径

1. 用户调用 `send*`、`recv*` 或对 socket fd 读写；
2. syscall/net 复制用户参数，定位 socket `File`；
3. socket 类型实现将数据交给 smoltcp、Unix socket 队列或 loopback；
4. `SERVICE`/设备轮询推进协议状态，事件接口唤醒等待者；
5. 接收数据经受检用户内存复制返回调用方。

真实网卡回归应区分 loopback 基线与经 `eth0` 的外部往返，因为只有后者覆盖 VirtIO RX/TX 和 QEMU 网络后端。

== 驱动模型

驱动目录提供设备容器、磁盘、控制台和网络设备抽象。VirtIO 块设备为 ext4 提供块读写；VirtIO-net 实现 `NetDriverOps` 所需的收发与缓冲区管理。RISC-V 使用 virt 平台接入，LoongArch 的 VirtIO 设备经 PCI 枚举和配置空间访问发现。

DMA 缓冲区、virtqueue 描述符和设备寄存器访问必须符合平台对齐与可见性要求。启动阶段的栈、静态堆、直接映射和 CMA 物理页范围也影响驱动可靠性，尤其在大内存及 PCI 平台下不能假定物理内存连续。

== 控制台、时钟与中断

console/logger 提供早期输出和分级日志；架构时间模块设置时钟频率与下一次 tick。中断处理经 trap 进入后再交给设备或调度相关路径。轮询仍是网络服务推进的重要机制，设备中断优化不应破坏现有轮询的进度保证。
