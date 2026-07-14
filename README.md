# Ya2yOS

![alt text](Docs/img/华南理工大学.png)
项目成员：饶晓杰

本项目主要基于2025年塔特林设计局的参赛作品[TatlinOS](https://gitlab.eduxiji.net/educg-group-36002-2710490/T202510487995221-883)的2025-tatlin分支。

截至6.19本项目的得分情况如表所示：

| 测试点 | glibc-la | glibc-rv | musl-la | musl-rv | 总分 |
| :--- | ---: | ---: | ---: | ---: | ---: |
| basic | 102 | 102 | 102 | 102 | 408 |
| busybox | 53 | 53 | 53 | 53 | 212 |
| cyclictest | 3.0 | 1.0 | 2.0 | 1.0 | 7.0 |
| iozone | 20.0 | 20.0 | 20.0 | 20.0 | 80.0 |
| iperf | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 |
| libcbench | 31.41374572024125 | 29.577473537091695 | 28.978582236286172 | 27.0 | 116.96980149361912 |
| libctest | - | - | 217 | 217 | 434 |
| lmbench | 18.0 | 36.57334929121593 | 36.98254922371577 | 36.648410766327274 | 128.204309281259 |
| ltp | 5664 | 5568 | 5641 | 5473 | 22346 |
| lua | 9 | 9 | 9 | 9 | 36 |
| netperf | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 |
| 总分 | 5900.413745720241 | 5819.150822828307 | 6109.9611314600015 | 5938.648410766327 | 2422.174110774878 |

食用指南参考[使用指南](./Docs/使用指南.md)
项目文档和开发日志均位于 [Docs](./Docs/) 目录下。
[PPT](Docs/pre_slides.pdf) 可以帮助你迅速了解这个项目的情况。
整个内核详细的设计文档请参考[内核设计文档](./Docs/ya2yos/typst/main.typ)；在
仓库根目录中执行 `typst compile --root . Docs/ya2yos/typst/main.typ
Docs/ya2yos/typst/ya2yos-kernel-design.pdf` 可生成 PDF。
原有 Markdown 设计材料保留在 [Docs/ya2yos/](./Docs/ya2yos/) 供追溯。
在引入skills后，AI的每一次修改均会在Docs/初赛文档/AI_INTERACTION.md 和 Docs/初赛文档/ai.log 这两个文件中记录。前者注重人机交互过程，后者注重AI的修改范围。与AI的主要交互方式是让AI通过输出日志进行修改，本人只做最后的原因分析和验收。
[bug修复文档](./Docs/初赛文档/problem/)
文档中的图均在[uml](./Docs/uml/)目录下。
[演示视频](https://1839796361.share.123pan.cn/123pan/ihR2Td-F5MpH?pwd=ya2y#)

主要开发分支在nightly, release分支只记录可以在评测机正常跑分的版本。

参考项目及书籍:

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
