# 调度选择 perf 计数的原子开销

## 背景

BuildStorm perf 路径会在每次调度选择时记录 `SCHEDULER_SELECTIONS`，并每 4096 次触发一次快照检查。
该路径调用频率很高，累计选择次数可超过十亿次。

## 现象

旧实现先用 `fetch_add` 增加选择计数，再用一次 `load` 读取最新值判断是否到达快照边界。计数统计本身正确，
但每次调度选择都多执行一次 Relaxed 原子读取，放大了 perf 配置下的调度器测量开销。

## 根因

递增结果与边界判断被拆成两个原子操作。该读操作不提供额外同步语义，却位于调度热路径中。

## 修复

`record_scheduler_selection()` 改用 `fetch_add(1, Ordering::Relaxed) + 1` 直接得到递增后的序号，再按原有
`0x0fff` 掩码调用 `maybe_report()`。自选计数、报告周期和输出字段均保持不变。

## 涉及文件

- `os/src/utils/perf/scheduler.rs`

## 验证

本轮未运行 perf kernel 或 BuildStorm，只执行了暂存区及文档的 `git diff --check`。后续应对比调度选择计数守恒、
报告边界和 perf 构建结果，并在固定 workload 下测量该微优化是否值得保留。
