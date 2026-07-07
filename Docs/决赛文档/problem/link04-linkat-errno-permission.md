# link04 linkat errno 与父目录权限修复

## 背景

LTP `link04` 是 `link(2)` 的负向用例，覆盖旧路径和新路径为空、路径过长、路径中间分量不是目录、目标已存在，以及非 root 用户在父目录缺少写或搜索权限时的错误码。

## 现象

新的 `log.ans` 中，musl 和 glibc 的 `link04` 都有 6 个失败：

- `link("", "nefile")` 返回 `EPERM`，期望 `ENOENT`。
- 旧路径或新路径为超长 pathname 时返回 `ENOENT`，期望 `ENAMETOOLONG`。
- `link("regfile", "")` 返回 `EEXIST`，期望 `ENOENT`。
- 在缺少写权限或搜索权限的目录下创建 hard link 返回成功，期望 `EACCES`。

## 分析

`sys_linkat()` 读取用户字符串后直接调用 `get_abs_path()`。当路径为空且不是 `AT_EMPTY_PATH` 语义时，`get_abs_path()` 会把空相对路径解析成当前工作目录，导致旧路径空串被当作目录并返回 `EPERM`，新路径空串被当作已存在路径并返回 `EEXIST`。

超长路径用例也没有在 `linkat` 入口做长度检查。当前 `read_user_cstr()` 最多读取 `MAX_PATH_LEN` 字节；当用户字符串达到上限且没有提前遇到 `\0` 时，会返回被截断到 `MAX_PATH_LEN` 的字符串。后续底层查找只看到一个普通不存在路径，因此返回 `ENOENT`。

权限失败用例中，LTP 先以 root 建立目录和文件，再切到 `nobody`。`linkat` 普通分支只检查源文件存在、源 inode 不是目录和新路径是否已存在，随后直接调用底层 `hard_link()`，没有检查旧路径父目录搜索权限，也没有检查新路径父目录写和搜索权限。

## 根因

`sys_linkat()` 缺少 Linux 兼容的路径参数预检和 hard link 创建前的父目录权限检查，导致错误码优先级被 `get_abs_path()`、`open()` 和底层 ext4 查找结果覆盖。

## 修复

在 `os/src/syscall/fs/ctl.rs` 中新增 `linkat` 专用 helper：

- `check_link_path()`：空路径在非 `AT_EMPTY_PATH` 场景返回 `ENOENT`；长度达到内核路径读取上限或单个路径分量超过 255 返回 `ENAMETOOLONG`。
- `parent_path_of()`：从绝对路径中取得父目录路径。
- `check_parent_permission()`：打开父目录并按当前 effective uid/gid 检查搜索权限；创建目录项时额外要求写权限；root 保持绕过普通权限位。

`sys_linkat()` 在解析绝对路径前先校验原始用户路径，在普通 hard link 分支检查旧路径父目录搜索权限和新路径父目录写/搜索权限；在 `AT_EMPTY_PATH` materialize 分支检查新路径父目录写/搜索权限。

## 涉及文件

- `os/src/syscall/fs/ctl.rs`

## 验证

已执行：

```text
make
timeout 120s make run > /tmp/link04-after-fix.log 2>&1
```

结果：

- 默认 LoongArch64 `make` 通过，只有既有 `smoltcp` vendor warning。
- LoongArch64 musl `link04`：14 项 `TPASS`，summary 为 `passed 14 failed 0 broken 0`。
- LoongArch64 glibc `link04`：14 项 `TPASS`，summary 为 `passed 14 failed 0 broken 0`。
