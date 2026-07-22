# LoongArch BuildStorm 稀疏 EXT4 文件读取破坏动态链接数据

## 背景

BuildStorm 正式脚本会在 Debian glibc 根文件系统中执行 Rust 工具链、创建
MINIBUILD，并最终编译 ArceOS 示例。LoongArch64 的 `/root/.cargo/bin/rustup`
是一个稀疏 EXT4 文件；`rustc`、`cargo` 和动态加载器会读取其中较大的 ELF
区间。

此前全量正式 runner 已恢复为参考脚本规定的 `BUILDSTORM_*` 输出契约，因此
judge 的 0 分结果需要按真实运行失败处理，而不是修改 judge 或伪造成功标记。

## 现象

初始 LoongArch64 `log.ans` 在 Rust 工具链启动时立即出现：

```text
rustc: error while loading shared libraries: unexpected reloc type 0x00dd8170
BUILDSTORM_TOOLCHAIN fail
BUILDSTORM_MINIBUILD fail
BUILDSTORM_COMPILE mode=multi ok=false rc=127 ... arch=riscv64
```

`judge_buildstorm-glibc.py` 因此给出 0 分是符合协议的：前两项没有 canonical
`ok` 标记，编译项也真实失败。

另一个独立但会放大问题的现象是，旧 `uname -m` 返回 `RISC-V64`。该字符串不匹配
参考脚本中的 `riscv64`/`loongarch64` 分支，于是 LoongArch64 guest 落入默认的
RISC-V target 分支，日志才会显示 `arch=riscv64`。

## 分析

镜像中 `rustup` 的关键 extent 形状为：

```text
logical block 0-327      -> 已分配物理块
logical block 328        -> hole
logical block 329-3293   -> 已分配物理块
logical block 3294       -> hole
```

文件系统块大小为 4096 B，因此 `0x148000 / 4096 = 328` 应当读取为全零，紧随其后
的 `0x149000 / 4096 = 329` 才含有 relocation 数据。原始文件中
`0x148340` 为零，而 `0x149340` 含有 `0x0000000000dd8170`。

旧 `ext4_fread()` 把 `fblock_start == 0` 同时用作“尚未初始化的连续读取 run”和
“稀疏/未写入逻辑块”。当它连续处理 `328 -> 0` 与 `329 -> X` 时，会把物理块 `X`
的内容错误读到逻辑块 328；后续又正确读到逻辑块 329。于是两个逻辑页都出现
`0x00dd8170`，glibc 动态加载器将其当成 relocation type 后报出上述错误。

尾部非整块读取路径同样会把 `fblock == 0` 当成物理块 0 读取。该问题发生在 ELF
内容映射前，因此不是 LoongArch TLB、页表、ELF ABI 或动态链接器本身的错误。

此外，`lwext4_rust/build.rs` 原先仅在 archive 不存在时调用 CMake。修改 C 源码后，
已有的 `liblwext4-loongarch64.a` 仍可能被复用，导致源码修复没有实际进入内核。

## 根因

主根因是 lwext4 的 `ext4_fread()` 没有把 sparse/unwritten block 作为零填逻辑块
独立处理，导致连续物理块聚合错误并破坏用户态 ELF 内容。

归档新鲜度缺失会使这个修复被旧静态库掩盖；`uname` 的非标准 machine 字符串则会令
正式脚本选择错误的交叉 target，但两者都不是 `0x00dd8170` 的直接来源。

## 修复

`ext4_fread()` 现在：

1. 对整块读取中的 `fblock == 0` 使用 `memset(..., 0, block_size)`，并完整推进
   buffer、文件偏移、读取计数和逻辑块索引。
2. 只合并物理块号非零且连续的读取 run；lookahead 遇到 hole 或不连续块时不消费
   该逻辑块，留给下一轮独立处理。
3. 对尾部非整块的 hole 同样零填，避免读取物理块 0。

`build.rs` 递归比较 `src/`、`include/`、CMake 文件和输出 archive 的修改时间，C
源码或头文件变化后会重建 `liblwext4-<arch>.a`；同时对这些输入声明
`cargo:rerun-if-changed`。

`sys_uname()` 按编译架构返回精确的小写 `riscv64` 或 `loongarch64`，使正式脚本进入
正确 target 分支。正式 BuildStorm 脚本和 judge 均保持不变。

## 涉及文件

- `crates/lwext4_rust/c/lwext4/src/ext4.c`
- `crates/lwext4_rust/build.rs`
- `os/src/syscall/sys/system.rs`
- `Docs/决赛文档/problem/loongarch-buildstorm-sparse-ext4-read-corruption.md`
- `Docs/决赛文档/problem/README.md`
- `Docs/决赛文档/开发日志.md`
- `Docs/决赛文档/ai.log`
- `Docs/决赛文档/AI_INTERACTION.md`

## 验证

官方 Docker image 中执行：

```bash
make -C crates/lwext4_rust/c/lwext4 musl-generic ARCH=loongarch64
```

重建后的 `ext4.c.o` 保持：

```text
Machine: LoongArch
Flags: 0x43, DOUBLE-FLOAT, OBJ-v1
GCC: (GNU) 13.2.0
```

且当前 `ext4_fread` 符号大小为 1160 B，和修复前 object 的 1012 B 不同，确认新 C
实现已进入 archive。

新的 LoongArch64 QEMU `log.ans` 已无 `unexpected reloc type`，并输出：

```text
BUILDSTORM_TOOLCHAIN ok
BUILDSTORM_MINIBUILD ok
```

`python3 scripts/judge_buildstorm-glibc.py < log.ans` 当前为 20/180：QEMU 在
`cargo build -p tg-xtask` 预构建阶段收到外层终止信号，日志只有
`QEMU: Terminated`，尚未生成 `BUILDSTORM_COMPILE`。因此本复盘不把 compile/time
项或完整 BuildStorm 宣称为通过；应在无过短外层 timeout 的环境中继续全量回归。

日志中的：

```text
<jemalloc>: MADV_DONTNEED does not work (memset will be used instead)
<jemalloc>: (This is the expected behaviour if you are running under QEMU)
```

是 jemalloc 在 QEMU 下的预期回退提示，不是内核错误，也不影响本问题的正确性结论。

`git diff --check` 已通过。RISC-V 尚未在本次 C 库重建后重新运行完整 BuildStorm，
需要作为后续回归执行。
