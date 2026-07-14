# Ya2yOS Typst 内核设计文档

`main.typ` 是唯一入口，章节位于 `chapters/`。它是面向外部读者的正式设计报告源文件；
Markdown 仅保留给 README、开发日志、问题复盘和轻量索引。报告包含固定版本、代码快照、
源码追溯表和参考文献，修改内核模块边界时须同步更新相应章节。

UML 图示位于仓库的 `Docs/uml/`，因此需以仓库根目录作为 Typst project root。执行以下命令生成 PDF：

```bash
cd /path/to/oskernel2026-ya2yos
typst compile --root . --pdf-standard a-2u \
  Docs/ya2yos/typst/main.typ Docs/ya2yos/typst/ya2yos-kernel-design.pdf
```

`--pdf-standard a-2u` 用于生成面向长期存档和外部交换的 PDF/A-2u 文件。生成的
`ya2yos-kernel-design.pdf` 是本地构建产物，不纳入版本控制。原有的 `Docs/ya2yos/*.md`
保留为历史材料；当前设计文档以本 Typst 工程为准。
