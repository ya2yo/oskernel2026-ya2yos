# Ya2yOS Typst 内核设计文档

`main.typ` 是唯一入口，章节位于 `chapters/`。它是面向外部读者的正式设计报告源文件；
Markdown 仅保留给 README、开发日志、问题复盘和轻量索引。报告包含固定版本、代码快照、
源码追溯表和参考文献，修改内核模块边界时须同步更新相应章节。

答辩演示源文件位于 `slides/defense.typ`，并可独立生成 PDF 或 PPTX。演示中的量化结果
只采用有可追溯日志支持的定向观测，完整 BuildStorm 成绩以官方 `ok=true elapsed_s=...`
输出为准。

```bash
cd /path/to/oskernel2026-ya2yos
typst compile --root . --pdf-standard a-2u \
  Docs/ya2yos/main.typ ya2yos-kernel-design.pdf

typst compile --root . Docs/ya2yos/slides/defense.typ Docs/ya2yos/slides/ya2yos-defense.pdf
node Docs/ya2yos/slides/generate-pptx.mjs
```

PPTX 生成器不依赖 npm 包，会直接生成可编辑的 Office Open XML 文字/版式对象；PDF 演示稿
仅依赖 Typst。

`--pdf-standard a-2u` 用于生成面向长期存档和外部交换的 PDF/A-2u 文件。生成的
`ya2yos-kernel-design.pdf` 是本地构建产物，不纳入版本控制。
