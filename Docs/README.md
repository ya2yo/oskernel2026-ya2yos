# 文档说明

[决赛文档](./决赛文档/)里面记录了决赛开发过程中的日志、AI 使用说明，以及按主题拆分的问题复盘（[problem/](./决赛文档/problem/)）。

[tatlinos](https://gitlab.eduxiji.net/T202510487995221/tatlin-os/-/tree/fix/2025_submit/Docs)原tatlinos的文档。
[ya2yos](./ya2yos/)里面介绍了修改后的新内核的架构。当前可编译的设计文档入口为
[ya2yos/typst/main.typ](./ya2yos/typst/main.typ)，其生成方式见[ya2yos/typst/README.md](./ya2yos/typst/README.md)。
面向外部的设计、架构与技术报告使用 Typst 源文件；README、开发日志和问题复盘仍使用 Markdown。
生成内核设计 PDF 时应从仓库根目录执行 `typst compile --root .`，以便加载 `Docs/uml/` 中的图示。

![乱码](./img/乱码.png)如图问题，需要切换到utf8格式。
