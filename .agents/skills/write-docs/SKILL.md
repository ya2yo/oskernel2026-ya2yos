---
name: write-docs
description: >-
  Ya2yOS 文档记录。用于写或更新开发日志、Docs/决赛文档/problem/ 问题复盘、
  尤其是完成功能、syscall 或 bug 修复后的记录。
---

# 写文档

目标：不同文档各写各的，不把长分析塞进开发日志，也不把 AI 对话塞进 problem。

## 写哪些

| 情况 | 开发日志 | problem/ |
|------|----------|----------|
| 新 syscall / 明显新功能 | 要 | 视复杂度 |
| 修通测例 / panic / 语义 bug | 要 | 要 |
| 只改 typo / fmt / obvious 一行 | 不要 | 不要 |
| 只咨询 AI，未改代码 | 不要 | 不要 |

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

## 检查

- 开发日志短。
- problem 能让后来者理解根因和修法。
- 最终回复列出写了哪些文档；没写某类文档要说明原因。
