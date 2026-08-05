# BuildStorm linker wrapper 的 shebang 解释器语义

## 背景

BuildStorm 的 `cargo xtask arceos build` 会让 Rust 直接执行
`/work/tgoskits/tmp/axbuild/std/linker-*-dynamic.sh`。这类 wrapper 的解释器由文件首行
`#!` 决定，不能根据文件名后缀推断。

## 现象

`server.ans` 在 `arceos-helloworld` 的最终链接阶段报告：

```text
linking with .../linker-riscv64gc-unknown-linux-musl-dynamic.sh failed: exit status: 2
...dynamic.sh: line 6: syntax error: unexpected "("
```

编译器和 `tg-xtask` 预构建均已完成，失败发生在 linker wrapper 启动后、真正调用 linker
之前。

## 分析

`os/src/syscall/task/execve.rs` 原先在 shebang 检查前按路径后缀处理所有 `.sh` 文件：将
目标路径替换成 `/musl/busybox`，并在参数前插入 `busybox sh`。这会绕过脚本自己的
`#!/bin/bash`，使 Rust 直接执行的 Bash wrapper 改由 BusyBox `sh` 解释。Bash 数组、进程
替换等语法在该解释器下会触发 `syntax error: unexpected "("`；因此错误表面上像脚本内容
损坏，实际是内核改变了 exec 的解释器选择。

项目已经有完整的 shebang 解析路径：读取 `#!` 行、重建解释器参数，并对 `/bin/sh` 兼容到
`/musl/busybox`。显式执行 `/bin/bash script` 的决赛 runner 也不应被后缀逻辑影响。

## 根因

`.sh` 后缀特判违反了 Linux `binfmt_script` 语义，覆盖了 shebang 指定的解释器。

## 修复

删除 `sys_execve()` 中的 `.sh -> busybox sh` 强制重写。脚本现在统一进入已有的 shebang
解析流程；无 shebang 的文本脚本返回 `ENOEXEC`，由调用者决定是否显式选择 shell。这样
BuildStorm linker wrapper 的 Bash 语法会由其声明的解释器解析，同时保留普通 POSIX
`#!/bin/sh` 脚本的 BusyBox 兼容行为。

## 涉及文件

- `os/src/syscall/task/execve.rs`

## 验证

- `rustfmt --edition 2021 --check os/src/syscall/task/execve.rs`：通过。
- `git diff --check`：通过。
- `make build-arch TARGET_ARCH=riscv64`、`make build-arch TARGET_ARCH=loongarch64`：通过。
- `timeout 180s make run TARGET_ARCH=riscv64`：日志输出 `BUILDSTORM_TOOLCHAIN ok`、
  `BUILDSTORM_MINIBUILD ok`，预构建推进到 `444/446`，随后因窗口到期退出（rc=124）。该窗口
  尚未进入 `arceos-helloworld` 最终 linker 调用，未观察到原错误但不能据此宣称完整链接通过，
  也没有完整 BuildStorm 结束标记。

全仓 `cargo fmt --manifest-path os/Cargo.toml -- --check` 仍受本次修改之外的
`memory_layout.rs` 与 `trap/mod.rs` 格式差异阻挡。

本轮没有修改外部只读的 `/work/tgoskits`，也没有把 `server.ans` 中的链接器错误误判为动态库
映射问题。
