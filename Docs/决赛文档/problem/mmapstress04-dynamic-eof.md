# LTP mmapstress04 文件扩展后的 mmap EOF 误判

## 背景

LTP `mmapstress04` 先将一个只有一页的文件按交错偏移建立私有只读映射，再通过另一
个 fd 将文件扩展到 384 页，最后读取所有映射页。Linux 允许映射先超出当时 EOF；文件
扩展后，扩展覆盖的页应正常读取。

## 现象

修复前根目录 `log.ans` 中 musl 和 glibc 两轮都在首次读取阶段被错误的 `SIGBUS` 终止：

```text
tst_test.c:1677: TBROK: Test killed by SIGBUS!
Summary:
passed   0
failed   0
broken   1
```

## 分析

`mmapstress04.c` 的 `setup()` 只建立匿名占位区；测试主体把文件的 1、3、5 页等交错页
映射到该区域，映射时文件长度仍为 4096 字节。随后 `rwfd` 顺序写入 384 页，最后的
逐字节检查才触发这些文件页的缺页。

当前 `MmapFile` 在建立 VMA 时保存 `mapped_file_size`，缺页处理用该值判断完整页是否
位于 EOF 之外。因此映射时不存在的页即使已经由后续写入覆盖，仍被判为 EOF 外页，trap
层按设计投递 `SIGBUS`。这与 Linux 允许文件在 mmap 后增长的语义冲突。

## 根因

文件映射的 EOF 检查使用了建立映射时的静态长度，而不是 backing inode 的当前长度。
原先为支持 unlink 后的打开映射而增加的快照，意外覆盖了合法的文件增长场景。

## 修复

- 删除 `MmapFile::mapped_file_size` 静态字段。
- `MmapFile::new()` 和 `replace()` 建立文件映射时先调用 inode `size()`，让 ext4 inode
  缓存当前长度；该长度在 unlink 后仍由打开 inode 保留。
- `mmap_file_page_beyond_eof()` 每次缺页都读取 inode 当前长度。ext4 的 `write_at()` 和
  `truncate()` 已同步更新该长度，因此文件扩展后的页可成功建立，截断后的完整外页仍
  产生 `SIGBUS`。

## 涉及文件

- `os/src/mm/map_area.rs`
- `os/src/mm/page_fault_handler.rs`
- `Docs/ya2yos/chapters/04-memory.typ`

## 验证

- `cargo fmt --manifest-path os/Cargo.toml --all`：通过。
- `git diff --check`：通过。
- `make TARGET_ARCH=riscv64`：通过；仅有既有 vendored `smoltcp` 未使用代码警告。
- RISC-V `log.ans`：musl/glibc 的 `mmapstress04` 均输出 `TPASS: blocks have expected data`，
  摘要均为 `passed 1 failed 0 broken 0 skipped 0 warnings 0`，最终 `shutdown!`。

本轮未运行 LoongArch64 QEMU 或完整 LTP 批量套件。
