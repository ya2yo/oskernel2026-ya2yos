# lwext4 Rust 分配器非法 Layout panic

## 背景

EXT4 pathname 并发拆锁后，Docker 内核可以正常编译，但运行 BuildStorm 时在 lwext4 的 Rust C 兼容分配器中 panic。

## 现象

日志为：

```text
[kernel] Panicked at crates/lwext4_rust/src/ulibc.rs:116
called `Result::unwrap()` on an `Err` value: LayoutError
```

## 分析

`free()` 将传入指针无条件解释为 `MemoryControlBlock`，读取其中的 size 后执行
`Layout::from_size_align(size + CTRL_BLK_SIZE, 8).unwrap()`。跨 C/Rust 静态链接边界使用裸 `malloc/free` 时，释放方、重复释放或越界写都会让该 size 不再可信；一旦溢出或超过 `Layout` 上限，内核 panic。

## 根因

lwext4 内核 archive 构建此前使用 `CONFIG_USE_USER_MALLOC=0`，分配接口依赖弱的裸 C 符号解析，无法明确保证所有 C 分配和释放都由同一 Rust allocator 配对；Rust free 路径也把损坏的元数据当作可信输入并 unwrap。

## 修复

- `MemoryControlBlock` 增加 magic 和 size 校验字段。
- `malloc/calloc/realloc/free` 增加整数溢出、布局构造和分配失败检查，非法块不再调用 `unwrap()`。
- 增加 `ext4_user_calloc/ext4_user_realloc` wrapper。
- 内核版 lwext4 的 build script 打开 `LWEXT4_USE_USER_MALLOC=ON`，生成 `CONFIG_USE_USER_MALLOC=1`，并在 `ext4_types.h` 声明四个显式接口，消除裸 `malloc/free` 的链接歧义。

## 涉及文件

- `crates/lwext4_rust/src/ulibc.rs`
- `crates/lwext4_rust/c/lwext4/CMakeLists.txt`
- `crates/lwext4_rust/c/lwext4/include/ext4_types.h`

## 验证

- Docker 内核交叉编译：维护者确认已正常完成。
- host 隔离 CMake 的 `LWEXT4_USE_USER_MALLOC=ON` 构建：`/tmp/lwext4-locksplit-build4` 的 `lwext4` target 通过，archive 仅保留 `ext4_user_*` 未定义符号，由 Rust wrapper 提供。
- `rustfmt --check crates/lwext4_rust/src/ulibc.rs`、`git diff --check` 通过。
- 当前宿主没有 Docker 交叉工具链，未在宿主复跑完整内核链接；host bcache lifecycle 可执行文件在当前工作区配置下出现独立 SIGSEGV，不能作为 Docker 内核回归结论。
