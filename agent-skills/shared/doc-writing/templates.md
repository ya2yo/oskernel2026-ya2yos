# 文档模板

四份文档各一份模板，复制后填空。详见 [SKILL.md](SKILL.md)。

---

## 模板 1：开发日志（简约）

```markdown
## 5.30

修复 accept02：setsockopt 未将 EADDRNOTAVAIL 返回用户态。详见 problem/。
```

---

## 模板 2：problem/ 单篇（问题详解）

新建 `Docs/初赛文档/problem/your-topic.md`，并更新 `problem/README.md` 索引。

```markdown
# accept02 组播与 setsockopt 错误传播

### 背景
…

### 现象
…

### 分析
…

### 修复
…

### 验证
…
```

---

## 模板 3：ai.log（AI 交互过程）

见 SKILL.md；涉及文件汇总中写 `Docs/初赛文档/problem/xxx.md`。

---

## 模板 4：AI_INTERACTION.md（合规条目）

描述中可写「详见 `ai.log` 日期条目」与 `problem/` 下对应文件。

---

## 模板 5：四份文档一次写完（检查用）

| 文档 | 写什么 |
|------|--------|
| 开发日志 | `修复 xxx。详见 problem/。` |
| problem/ | 新建 `xxx.md` + 更新 README 索引 |
| ai.log | AI 分析过程 |
| AI_INTERACTION | 合规摘要 |
