---
name: write-docs
description: >-
  Ya2yOS 文档记录。用于写或更新开发日志、Docs/决赛文档/problem/ 问题复盘、
  Docs/决赛文档/ai.log、Docs/决赛文档/AI_INTERACTION.md，尤其是完成功能、syscall 或 bug 修复后的记录。
---

# 写文档

目标：不同文档各写各的，不把长分析塞进开发日志，也不把 AI 对话塞进 problem。

## 写哪些

| 情况 | 开发日志 | problem/ | ai.log | AI_INTERACTION |
|------|----------|----------|--------|----------------|
| 新 syscall / 明显新功能 | 要 | 视复杂度 | 用 AI 则要 | 用 AI 则要 |
| 修通测例 / panic / 语义 bug | 要 | 要 | 用 AI 则要 | 用 AI 则要 |
| 只改 typo / fmt / obvious 一行 | 不要 | 不要 | 不要 | 不要 |
| 只咨询 AI，未改代码 | 不要 | 不要 | 要 | 要 |

## 开发日志

文件：`Docs/决赛文档/开发日志.md`

- 在末尾追加 `## M.DD` 或沿用当天已有小节。
- 1 到 3 句话，只写做了什么和结果。
- 需要细节时写“详见 problem/”，不要展开根因。

示例：

```markdown
## 6.13

修复 accept02：setsockopt 未将 EADDRNOTAVAIL 返回用户态。详见 problem/。
```

## problem/

目录：`Docs/决赛文档/problem/`

- 单问题单文件，文件名用英文 kebab-case。
- 新增文件后更新 `Docs/决赛文档/README.md` 索引。
- 内容写清：背景、现象、分析、根因、修复、涉及文件、验证。
- 不记录“用户问 AI 什么”，那属于 `ai.log`。

模板：

```markdown
# 标题

## 背景

## 现象

## 分析

## 根因

## 修复

## 验证
```

## ai.log

文件：`Docs/决赛文档/ai.log`

- 只有用了 AI 才写。
- 记录用户输入、AI 分析路径、采纳的结论、修改文件和验证结果。
- 新条目追加到文件末尾，风格跟随现有内容。

## AI_INTERACTION.md

文件：`Docs/决赛文档/AI_INTERACTION.md`

- 只有用了 AI 才写。
- 在合适阶段追加条目，包含工具/模型、场景、描述、关联 commit。
- 描述偏合规摘要，可指向 `ai.log` 对应条目，避免重复长文。

## 检查

- 开发日志短。
- problem 能让后来者理解根因和修法。
- 用了 AI 时 `ai.log` 与 `AI_INTERACTION.md` 都更新。
- 最终回复列出写了哪些文档；没写某类文档要说明原因。
