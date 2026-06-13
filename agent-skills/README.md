# Unified Agent Skills

本目录是 Codex 与 Claude Code 共用 skills 的维护入口。

## Source of truth

只编辑：

```text
agent-skills/shared/
```

生成目录：

```text
.codex/skills/
.claude/skills/
```

不要手动修改生成目录里的 skill。修改共享源后运行：

```bash
python3 scripts/sync_agent_skills.py
```

检查两端是否同步：

```bash
python3 scripts/sync_agent_skills.py --check
```

如果需要从某个已有 skills 目录重新初始化共享源：

```bash
python3 scripts/sync_agent_skills.py --init-from .claude/skills --force-init
```

