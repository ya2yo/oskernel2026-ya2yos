---
name: doc-writing
description: >-
  按四份文档分工撰写 Ya2yOS 开发记录：开发日志（简约）、problem/（问题详解）、
  ai.log、AI_INTERACTION.md。内核修改完成后必须执行（见 kernel-change）。
  用于写文档、记录 bug 修复或补充 AI 使用说明时。
---

# 开发文档撰写

> 内核代码改完后，何时写、写哪些，见 [kernel-change](../kernel-change/SKILL.md) 的「修改后：文档」一节。Agent 应在同一轮任务内完成文档，勿等用户提醒。

## 四份文档，各司其职

| 文档 | 路径 | 写什么 | 风格 |
|------|------|--------|------|
| **开发日志** | `Docs/初赛文档/开发日志.md` | 按日期记「做了什么」 | **简约**，一两句话 |
| **问题复盘** | `Docs/初赛文档/problem/` | 问题如何被发现、分析、解决 | **详细**，单文件单主题 |
| **AI 工作日志** | `ai.log`（项目根目录） | 与 AI 交互的完整过程 | 按次记录，偏技术流水 |
| **AI 合规记录** | `Docs/初赛文档/AI_INTERACTION.md` | 大赛要求的 AI 使用说明 | 分类 + 时间线，偏正式 |

**不要混写**：开发日志不写根因分析；problem 不写「我问了 AI 什么」；AI 过程不写进开发日志。

## 工作流

```
修完 bug / 完成功能
  ├─ 1. 开发日志.md     追加简约一行（或短段）
  ├─ 2. problem/          新建或更新对应主题 .md，并更新 problem/README.md 索引
  ├─ 3. ai.log          若用了 AI → 追加本次交互记录
  └─ 4. AI_INTERACTION.md  若用了 AI → 追加合规条目
```

用了 AI 时 **3 和 4 都要写**；没用 AI 则只写 1 和 2。

---

## 1. 开发日志 — 保持简约

文件：`Docs/初赛文档/开发日志.md`，在**末尾**追加。

### 风格要求

- 以 `## M.DD` 或 `## 第N周` 为标题
- **默认 1～3 句话**，口语化、记结果，不展开分析
- 需要指路时写「详见 problem/」，**不要把 problem 内容抄过来**
- 不要写「现象 / 根因 / 修改 / 验证」四段式（那是 problem 的活）
- 不要记录 AI 对话过程（那是 ai.log 的活）

### 好例子（本项目既有风格）

```markdown
## 5.19

解决ltp测试前几个测例死循环的bug。

## 5.22

针对cgroup_fj测试死循环的问题，通过大模型了解到是因为发送信号的那个任务挂了，所以将相关测试单独拎出来测试。

## 5.30

修复 accept02：setsockopt 错误未返回用户态。详见 problem/。
```

### 坏例子（太详细，应放到 problem/）

```markdown
## 5.30

### accept02

**现象**：TFAIL Multicast group was copied!
**根因**：sys_setsockopt 吞掉 Err...
**修改**：opt.rs 加 ? ...
```

### 何时可稍长

同一日期多个**独立**小项时，可用 `### 标题` 分条，每条仍保持 1～2 句（参考 `5.27` 中 iozone 那段的长度上限，不要再扩）。

---

## 2. problem/ — 写清楚解决了什么

目录：`Docs/初赛文档/problem/`（索引见 [README.md](../../Docs/初赛文档/problem/README.md)）

原 `problem.md` 仅保留跳转说明；**每篇问题单独一个文件**，文件名用英文 kebab-case（如 `accept02-mcast-setsockopt.md`）。

### 风格要求

- 文件内标题用 `# 简短标题`
- 写清：**背景、现象、分析过程、根因、修复、涉及文件、验证结果**
- 可贴关键日志、调用链、错误尝试与排除过程
- 面向「三个月后的自己或其他读代码的人」
- 不记录 AI 对话细节（放 ai.log）
- **新增后务必更新 `problem/README.md` 索引**

---

## 3. ai.log — 记录与 AI 的交互过程

文件：**项目根目录** `ai.log`（不是 Docs/ 下）

### 风格要求

- 按时间倒序或正序追加（与现有文件一致：新条目接在文末）
- 记录**过程**：用户给了什么输入 → AI 如何分析 → 采纳了哪些结论 → 改了什么
- 可含事件序列、表格、修改文件清单
- 比 problem 多「交互」维度，比 AI_INTERACTION 更细、更即时

### 推荐结构（与现有 ai.log 一致）

```markdown
## YYYY-MM-DD: 简短标题

### 问题发现
用户提供了什么（log.ans、报错、需求）。

### 根因分析
AI 的分析过程与结论（可含 TID/调用序列）。

### 修改内容
| 文件 | 位置 | 修改 |
|------|------|------|
| `os/src/...` | `fn foo` | 说明 |

### 涉及文件汇总
- 代码、文档、测试各改了哪些。
```

未使用 AI 的修复：**不要**写 ai.log。

---

## 4. AI_INTERACTION.md — 大赛合规记录

文件：`Docs/初赛文档/AI_INTERACTION.md`

### 风格要求

- 文首「声明」表：工具与模型列表保持更新
- 在「详细时间线」对应阶段下追加 `#### 标题（日期）`
- 每条固定字段：**工具/模型、场景、描述、关联 commit**
- 描述写清：输入、AI 产出、人工审核与采纳情况
- 可写「详见 `ai.log` YYYY-MM-DD 条目」避免重复粘贴
- 偏答辩/审查视角，不必贴完整日志

### 推荐条目格式

```markdown
#### accept02 setsockopt 日志分析（5.30）

- **工具/模型**：Cursor (Composer)
- **场景**：Bug 分析与定位
- **描述**：提供 log.ans，AI 分析 syscall 序列，对照 LTP accept02 源码确认
  MCAST_LEAVE_GROUP 应返回 EADDRNOTAVAIL；定位 opt.rs 未传播 set_option 错误；
  人工确认后采纳 `?` 修复。过程详见根目录 `ai.log` 2026-05-30 条目。
- **关联 commit**：`abc1234`
```

---

## 分工对照（避免重复）

| 内容 | 开发日志 | problem/ | ai.log | AI_INTERACTION |
|------|:--------:|:----------:|:------:|:--------------:|
| 今天修了什么（一句话） | ✓ | | | |
| 现象 / 根因 / 修复细节 | | ✓ | 简要 | |
| 用户问了 AI 什么 | | | ✓ | 摘要 |
| AI 分析步骤与采纳 | | | ✓ | ✓ |
| 大赛工具声明 | | | | ✓（文首表） |
| 修改文件列表 | 可选一句 | ✓ | ✓ | 可选 |

---

## 何时写 / 不写

| 情况 | 写哪些 |
|------|--------|
| 修完一个测例级 bug | 1 + 2；若用 AI 则 +3 +4 |
| 小 typo / 一行修复 | 仅 commit，四份都不写 |
| 用了 AI 但没改代码（纯咨询） | 3 +4，不写 1 |
| 构建方式变更 | `Docs/使用指南.md`，开发日志一句带过 |

---

## 检查清单

- [ ] 开发日志：简约，无技术分析长文
- [ ] problem/：新问题有独立文件且 README 索引已更新
- [ ] 用了 AI → `ai.log` 有过程记录
- [ ] 用了 AI → `AI_INTERACTION.md` 有合规条目
- [ ] 四份文档互不复制大段内容，用「详见 xxx」交叉引用

## 模板

可复制模板见 [templates.md](templates.md)。

## 相关技能

- [kernel-change](../kernel-change/SKILL.md) — 何时必须写文档
- [build-and-test](../build-and-test/SKILL.md) — log.ans 分析
- [debug-playbook](../debug-playbook/SKILL.md) — 常见 bug 模式
- [oskernel-conventions](../oskernel-conventions/SKILL.md) — 代码规范
