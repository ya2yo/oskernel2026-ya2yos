---
name: kernel-change
description: >-
  Ya2yOS 内核修改总流程：修改前读取对应领域 skill、改代码与验证、完成后按 doc-writing
  更新开发日志/problem/ai.log/AI_INTERACTION。用于实现 syscall、修 bug、网络/任务/FS
  改动，或用户要求改内核且需遵循项目规范与文档时。
---

# 内核修改工作流

**原则**：改内核 ≠ 只改 `.rs`。须先读对 skill、写完代码要验证、**非平凡改动必须补文档**。

完整文档规范见 [doc-writing](../doc-writing/SKILL.md)。

---

## 流程总览

```
1. 读 skill（本表「修改前」）
2. 读现有代码，小范围改动
3. 编译 + 跑测例（build-and-test）
4. 写文档（doc-writing，按改动类型）
5. 收尾检查（下方清单）
```

---

## 修改前：必读 / 选读 skill

| 改动类型 | 必读 | 建议再读 |
|----------|------|----------|
| **任意内核代码** | [oskernel-conventions](../oskernel-conventions/SKILL.md) + [placement.md](../oskernel-conventions/placement.md) | — |
| 重组目录 / 拆分模块 / 新建子系统 | [rust-project-layout](../rust-project-layout/SKILL.md) | placement.md |
| 新增/修改 syscall | [syscall-implementation](../syscall-implementation/SKILL.md) | debug-playbook |
| 网络（socket、驱动、组播） | [network-stack](../network-stack/SKILL.md) | [network-debug](../network-debug/SKILL.md) |
| LTP / initproc 测例失败 | [ltp-test-triage](../ltp-test-triage/SKILL.md) | debug-playbook / network-debug |
| futex / wait / clone / 信号 | [debug-playbook](../debug-playbook/SKILL.md) | oskernel-conventions |
| COW / 页表 / 缺页 | [debug-playbook](../debug-playbook/SKILL.md) | [dual-arch](../dual-arch/SKILL.md) |
| pipe / ext4 / 动态链接 | [debug-playbook](../debug-playbook/SKILL.md) | — |
| LoongArch 或双架构 | [dual-arch](../dual-arch/SKILL.md) | — |
| 编译、QEMU、log.ans | [build-and-test](../build-and-test/SKILL.md) | — |

Agent：**开始改 `os/src/` 前先按上表打开对应 SKILL.md**，不要凭通用 OS 知识臆测本项目实现。

**禁止**：先扫仓库、用「发现模块化目录」反推规范（如「`fs/files/epoll/` 很模块化，由此理解规范」）。必须以 [placement.md](../oskernel-conventions/placement.md) 为权威，代码目录仅作对照范例。

入口：本 skill + [skills README](../README.md)；Cursor 另见 `.cursor/rules/`

---

## 修改时：代码约束（摘要）

来自 [oskernel-conventions](../oskernel-conventions/SKILL.md)，**放错目录比写错逻辑更难查**：

- 先查 [placement.md](../oskernel-conventions/placement.md) 决策树
- `sys_*` → `syscall/`；协议语义 → `net/`；硬件 → `drivers/`；页表 ISA → `arch/`

- 用户指针：`copy_from_user` / `copy_to_user`，禁止 `translated_str().unwrap()`
- syscall 包装：`match { ... }?`，禁止吞掉 `set_option` / 内部 `Err` 后固定 `Ok(0)`
- 锁顺序：先 task，后 process（fd_table 在 process 内）
- 网络阻塞路径：须能调到 `poll_interfaces()`
- 双架构：改 `arch/` 或页表时考虑 RV 与 LA 两侧

---

## 修改后：验证

按 [build-and-test](../build-and-test/SKILL.md)：

```bash
make log                    # 或 TARGET_ARCH=loongarch64
make run                    # 保存 log.ans 若排查问题
```

- 单测 LTP：调 `initproc.rs` 的 `LTP_TEST_START` / `LTP_TESTS_PER_GROUP`
- 解读：`TPASS`/`TFAIL` 为准；`FAIL LTP CASE xxx : 0` 中 `: 0` 是退出码不是失败

---

## 修改后：文档（必须执行）

**凡以下情况，完成后调用 [doc-writing](../doc-writing/SKILL.md) 并实际写入文件：**

| 改动 | 开发日志 | problem/ | ai.log | AI_INTERACTION |
|------|:--------:|:--------:|:------:|:--------------:|
| 修通测例 / 修 panic / 语义 bug | ✓ | ✓ | 若用 AI ✓ | 若用 AI ✓ |
| 新 syscall 或明显新行为 | ✓ | 视情况 | 若用 AI ✓ | 若用 AI ✓ |
| 仅 fmt / typo / 一行 obvious fix | — | — | — | — |
| 改 Makefile / 镜像 / feature | ✓ 一句 | — | — | — |

### 文档动作（Agent 应主动完成）

1. **`Docs/初赛文档/开发日志.md`** — 末尾追加 `## M.DD`，1～3 句，可写「详见 problem/」
2. **`Docs/初赛文档/problem/`** — 新建 `topic-kebab.md` + 更新 [problem/README.md](../../Docs/初赛文档/problem/README.md)
3. **`ai.log`** — 本次用了 AI 辅助分析/生成代码时追加一节
4. **`Docs/初赛文档/AI_INTERACTION.md`** — 大赛合规条目，可指向 ai.log

**不要等用户提醒「记得写文档」**；与代码变更同一轮完成。

---

## 收尾检查清单

复制并逐项勾选：

```
代码
- [ ] 已读 placement.md，新代码在正确目录/文件
- [ ] 已读对应领域 skill，改动范围最小化
- [ ] copy_from_user / 错误传播 / 锁顺序符合规范
- [ ] make（+ 目标 arch）通过
- [ ] 相关测例已跑，结果符合预期

文档
- [ ] 开发日志已追加（若属「必须写文档」）
- [ ] problem/ 新文件 + README 索引（若有技术复盘价值）
- [ ] 用了 AI → ai.log + AI_INTERACTION.md
- [ ] 未把长分析塞进开发日志
```

---

## 快速决策树

```
用户要改内核
  ├─ 先读 kernel-change（本文件）+ oskernel-conventions
  ├─ 按改动类型读上表「修改前」skill
  ├─ 实现 + make run 验证
  └─ 非平凡？→ doc-writing 四份文档
```

---

## 相关技能索引

| 技能 | 路径 |
|------|------|
| 本工作流 | kernel-change/SKILL.md |
| 文档 | doc-writing/SKILL.md |
| 规范 | oskernel-conventions/SKILL.md |
| 全部列表 | [README.md](../README.md) |
