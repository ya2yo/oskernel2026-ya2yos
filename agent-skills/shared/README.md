# Ya2yOS Agent Skills

本目录包含面向本仓库（2026 OS 竞赛内核）的 Agent 技能。

## 统一维护方式

`agent-skills/shared/` 是唯一维护源；`.codex/skills/` 与 `.claude/skills/`
由脚本生成。

修改 skill 时只改共享源，然后运行：

```bash
python3 scripts/sync_agent_skills.py
python3 scripts/sync_agent_skills.py --check
```

不要直接改 `.codex/skills/` 或 `.claude/skills/` 下的生成文件。

## 如何让 Agent 稳定加载

| 工具 | 入口 |
|------|------|
| **Codex** | `.codex/skills/` 中的 [kernel-change/SKILL.md](./kernel-change/SKILL.md) + [placement.md](./oskernel-conventions/placement.md) |
| **Claude Code** | `.claude/skills/` 中的 [kernel-change/SKILL.md](./kernel-change/SKILL.md) + [placement.md](./oskernel-conventions/placement.md) |
| **Cursor** | [.cursor/rules/](../../.cursor/rules/)（自动）+ 上表 skill |
| **手动** | 对话中写：「按 kernel-change + placement.md 改 …」 |

**不要**依赖 Agent 扫代码「发现」规范；必须先读 [placement.md](./oskernel-conventions/placement.md)。

## 入口（改内核从这里开始）

| 技能 | 路径 | 用途 |
|------|------|------|
| **内核修改工作流** | [kernel-change/SKILL.md](./kernel-change/SKILL.md) | **改代码前读哪些 skill、改后如何验证、必须写哪些文档** |

## 技能索引

### 工作流型

| 技能 | 路径 | 用途 |
|------|------|------|
| 内核修改工作流 | [kernel-change/SKILL.md](./kernel-change/SKILL.md) | 总流程：skill + 验证 + 文档 |
| 编译测试 | [build-and-test/SKILL.md](./build-and-test/SKILL.md) | make、QEMU、log.ans |
| LTP 排查 | [ltp-test-triage/SKILL.md](./ltp-test-triage/SKILL.md) | LTP 单测、黑名单、输出解读 |
| 网络调试 | [network-debug/SKILL.md](./network-debug/SKILL.md) | virtio、setsockopt、组播 |
| 调试手册 | [debug-playbook/SKILL.md](./debug-playbook/SKILL.md) | futex/COW/pipe 等常见 bug |
| 双架构 | [dual-arch/SKILL.md](./dual-arch/SKILL.md) | RISC-V vs LoongArch 差异 |
| 文档撰写 | [doc-writing/SKILL.md](./doc-writing/SKILL.md) | 开发日志 / problem/ / ai.log / AI_INTERACTION |

### 知识型

| 技能 | 路径 | 用途 |
|------|------|------|
| 开发规范 | [oskernel-conventions/SKILL.md](./oskernel-conventions/SKILL.md) | 代码风格、**模块放置** |
| 目录速查 | [oskernel-conventions/placement.md](./oskernel-conventions/placement.md) | 防代码写错位置 |
| Rust 模块结构 | [rust-project-layout/SKILL.md](./rust-project-layout/SKILL.md) | 按 Rust 规范拆分/重组 mod 树 |
| 系统调用 | [syscall-implementation/SKILL.md](./syscall-implementation/SKILL.md) | 新增/修改 syscall |
| 网络架构 | [network-stack/SKILL.md](./network-stack/SKILL.md) | smoltcp 模块地图 |

## 使用建议

| 场景 | 推荐技能（按顺序） |
|------|---------------------|
| **任何内核修改** | **kernel-change** → 领域 skill → **doc-writing** |
| 首次跑通 | build-and-test |
| 新 syscall / 新模块 | kernel-change → **placement.md** → syscall-implementation → doc-writing |
| 重组目录 / 拆分文件 | kernel-change → **placement.md** → **rust-project-layout** → doc-writing |
| LTP 失败 | kernel-change → ltp-test-triage → debug-playbook / network-debug → doc-writing |
| 网络问题 | kernel-change → network-debug → network-stack → doc-writing |
| RV 过 LA 不过 | kernel-change → dual-arch → doc-writing |
| 只写文档 | doc-writing |

## 目录结构

```
agent-skills/shared/
├── README.md
├── kernel-change/SKILL.md    ← 入口
├── build-and-test/SKILL.md
├── ltp-test-triage/SKILL.md
├── network-debug/SKILL.md
├── debug-playbook/SKILL.md
├── dual-arch/SKILL.md
├── doc-writing/
│   ├── SKILL.md
│   └── templates.md
├── oskernel-conventions/
│   ├── SKILL.md
│   └── placement.md          ← 代码放哪
├── rust-project-layout/
│   ├── SKILL.md              ← Rust mod 怎么组织
│   └── patterns.md
├── syscall-implementation/SKILL.md
└── network-stack/SKILL.md

.codex/skills/        ← 由 shared 生成，供 Codex 使用
.claude/skills/       ← 由 shared 生成，供 Claude Code 使用
../../.cursor/rules/  ← Cursor 自动规则（workflow + placement）
```
