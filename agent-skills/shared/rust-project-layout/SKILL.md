---
name: rust-project-layout
description: >-
  按 Rust 模块规范组织 Ya2yOS 项目结构：拆分/合并文件、新建子模块、重构目录、
  pub use 与可见性。用于重组代码、新增子系统目录、文件过大需拆分、或用户要求
  按 Rust 规范整理模块时。须配合 placement.md 确定领域目录。
---

# Rust 项目结构组织

> **领域放哪**（syscall / net / fs …）：先读 [placement.md](../oskernel-conventions/placement.md)  
> **文件怎么拆、mod 怎么写**：读本 skill + [patterns.md](patterns.md)  
> 改内核全流程：[kernel-change](../kernel-change/SKILL.md)

---

## 定位

| 文档 | 回答的问题 |
|------|------------|
| [placement.md](../oskernel-conventions/placement.md) | 这段逻辑属于哪个**顶层模块/子系统**？ |
| **本 skill** | 在该目录内如何用 **Rust 模块惯例** 组织文件与子模块？ |
| [patterns.md](patterns.md) | 具体模式、反模式与本仓库范例 |

**禁止**：只按 Rust 书上的「通用 crate 布局」臆测，忽略 Ya2yOS 的 syscall 薄层 + 领域语义分层。

---

## Agent 必读顺序

1. [placement.md](../oskernel-conventions/placement.md) — 决策树，确定目标目录
2. **本文件** — 工作流与检查清单
3. 目标目录内 **已有** `mod.rs` / 同级 `.rs` — 对齐声明与 `pub use` 风格
4. 需要范例时读 [patterns.md](patterns.md)

---

## 重组 / 新建模块工作流

复制并逐项执行：

```
结构组织
- [ ] 1. placement 决策树：确认代码属于哪个顶层/子目录
- [ ] 2. 读目标目录现有 mod 树，不引入第二种组织风格
- [ ] 3. 选模块形态（单文件 / 目录+mod.rs），见 patterns.md「模块形态」
- [ ] 4. 按职责拆文件；单文件 ~400 行且可拆则拆，禁止无意义 helpers.rs
- [ ] 5. 更新 mod.rs：mod 声明 + 精选 pub use + //! 文档
- [ ] 6. 设可见性：跨顶层用 pub；crate 内 pub(crate)；其余私有
- [ ] 7. 架构/feature 差异用 #[cfg]，长 cfg 块下沉到 arch/ 而非 syscall
- [ ] 8. make（+ 目标 arch）通过
- [ ] 9. 向用户说明：新结构、每个文件职责、为何符合 placement + Rust 惯例
```

### 新建子系统（syscall 薄层 + 领域语义）

参照 epoll（见 placement「参考实现」）：

| 层 | 路径模式 | 放什么 |
|----|----------|--------|
| 入口 | `syscall/<主题>/xxx.rs` | `sys_*`、用户指针、超时/挂起循环 |
| 语义 | `fs/files/xxx/` 或 `net/` 等 | 状态机、trait 实现、全局表 |

不要在 syscall 文件里堆业务状态机；也不要为「模块化」把 `sys_*` 拆进 `fs/`。

---

## 快速决策：单文件还是子目录？

```
仅 1 个类型 + 少量私有 fn，且 < ~200 行？
  └─ 是 → xxx.rs，在父 mod.rs 中 mod xxx;

多个子职责（ctl / wait / registry …）或已有同名目录范例？
  └─ 是 → xxx/mod.rs + 按职责命名的子 .rs

同一主题 syscall 已有 stat.rs / io.rs 等？
  └─ 是 → 追加到现有文件，不新建平行文件
```

细则与反模式 → [patterns.md](patterns.md)

---

## 输出要求（Agent 必须说明）

重组或新建模块后，在回复中写清：

1. **placement 依据**：对照决策树哪一条
2. **目录树**：变更前后的 `mod` 结构（ASCII 即可）
3. **每个新/改文件的职责**（一行）
4. **`mod.rs` 变更**：新增 `mod` 声明与 `pub use` 理由
5. **未移动的内容**：若有代码故意留在原处，说明原因

---

## 与 oskernel-conventions 的关系

| 主题 | 文档 |
|------|------|
| 命名、错误处理、锁顺序 | [oskernel-conventions/SKILL.md](../oskernel-conventions/SKILL.md) |
| 目录决策树、误放表 | [placement.md](../oskernel-conventions/placement.md) |
| Rust mod / pub use / 拆分 | **本 skill** + [patterns.md](patterns.md) |

---

## 相关技能

| 场景 | 技能 |
|------|------|
| 新增 syscall | [syscall-implementation](../syscall-implementation/SKILL.md) |
| 网络子模块 | [network-stack](../network-stack/SKILL.md) |
| 双架构 cfg | [dual-arch](../dual-arch/SKILL.md) |
| 改完验证 | [build-and-test](../build-and-test/SKILL.md) |
| 非平凡重构后文档 | [doc-writing](../doc-writing/SKILL.md) |
