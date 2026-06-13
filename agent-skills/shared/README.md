# Ya2yOS Agent Skills

本目录只保留面向本仓库（2026 OS 竞赛内核）的 3 个高频入口：

1. 添加功能 / 实现 syscall
2. 修复 bug
3. 写文档

## 统一维护方式

`agent-skills/shared/` 是唯一维护源；`.codex/skills/` 与 `.claude/skills/`
由脚本生成。

修改 skill 时只改共享源，然后运行：

```bash
python3 scripts/sync_agent_skills.py
python3 scripts/sync_agent_skills.py --check
```

不要直接改 `.codex/skills/` 或 `.claude/skills/` 下的生成文件。

## 技能索引

| 技能 | 路径 | 用途 |
|------|------|------|
| 添加功能 / Syscall | [add-syscall-feature/SKILL.md](./add-syscall-feature/SKILL.md) | 新功能、新 syscall、syscall 语义修改 |
| 修复 Bug | [fix-bug/SKILL.md](./fix-bug/SKILL.md) | panic、LTP 失败、卡死、语义错误、日志分析 |
| 写文档 | [write-docs/SKILL.md](./write-docs/SKILL.md) | 开发日志、problem/、ai.log、AI_INTERACTION |

## 使用建议

| 场景 | 推荐技能（按顺序） |
|------|---------------------|
| 新 syscall / 新功能 | add-syscall-feature → write-docs |
| 修 bug / 跑测例失败 | fix-bug → write-docs |
| 只写记录 | write-docs |

## 目录结构

```
agent-skills/shared/
├── README.md
├── add-syscall-feature/SKILL.md
├── fix-bug/SKILL.md
└── write-docs/SKILL.md

.codex/skills/        ← 由 shared 生成，供 Codex 使用
.claude/skills/       ← 由 shared 生成，供 Claude Code 使用
```
