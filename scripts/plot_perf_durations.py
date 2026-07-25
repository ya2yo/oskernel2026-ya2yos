#!/usr/bin/env python3
"""将 Ya2yOS 关机后的 perf 耗时汇总绘制为 SVG 饼图。

默认读取最后一个 ``shutdown!`` 之后的 ``syscall_duration`` 指标。默认不包含
``*_active`` 指标，因为它们是对应 syscall 耗时的嵌套子集；旧日志若同时有
``wait``/``accept``/``read`` 和对应的 ``*_active`` 指标同时存在时，会优先使用实际运行的
``wait_active``/``accept_active``/``read_active``。其中 ``read_active`` 仅覆盖普通文件
读取的活动区间。

示例：
    python3 scripts/plot_perf_durations.py log.ans
    python3 scripts/plot_perf_durations.py log.ans --group clone_duration
    python3 scripts/plot_perf_durations.py log.ans --include-active -o perf.svg
    python3 scripts/plot_perf_durations.py log.ans --list-groups

输出是无需第三方 Python 包的 SVG 文件，可直接在浏览器中打开。
"""

from __future__ import annotations

import argparse
import html
import math
import os
import re
import sys
from dataclasses import dataclass, replace
from pathlib import Path


ANSI_ESCAPE = re.compile(r"\x1b\[[0-?]*[ -/]*[@-~]")
DURATION_RE = re.compile(
    r"^\[perf\]\s+(?P<group>\S+_duration)\s+"
    r"(?P<label>[^\s(]+)\(samples=(?P<samples>\d+)\s+"
    r"total_us=(?P<total_us>\d+)\s+max_us=(?P<max_us>\d+)\)\s*$"
)

COLORS = (
    "#2563eb",
    "#dc2626",
    "#16a34a",
    "#d97706",
    "#7c3aed",
    "#0891b2",
    "#be123c",
    "#4f46e5",
    "#65a30d",
    "#c2410c",
    "#9333ea",
    "#0f766e",
    "#e11d48",
    "#475569",
    "#ca8a04",
    "#0369a1",
    "#9f1239",
    "#3f6212",
)


@dataclass(frozen=True)
class Duration:
    """一个 perf ``*_duration`` 汇总项。"""

    group: str
    label: str
    samples: int
    total_us: int
    max_us: int
    line_no: int


def clean_line(raw_line: str) -> str:
    """删除终端控制字符和 NUL，保留可供正则解析的日志内容。"""

    return ANSI_ESCAPE.sub("", raw_line).replace("\0", "").strip()


def parse_final_snapshot(log_path: Path) -> tuple[int, list[Duration]]:
    """返回最后一个 ``shutdown!`` 后的 perf 指标及该行号。"""

    try:
        with log_path.open("r", encoding="utf-8", errors="replace") as log_file:
            lines = list(log_file)
    except OSError as error:
        raise ValueError(f"无法读取 {log_path}: {error}") from error

    shutdown_index = -1
    for index, raw_line in enumerate(lines):
        if clean_line(raw_line) == "shutdown!":
            shutdown_index = index

    if shutdown_index < 0:
        raise ValueError(f"{log_path} 中未找到 shutdown!，无法定位最终 perf 快照")

    # A duplicated perf line should not create two chart sectors. Keeping the
    # last occurrence is consistent with this being a final aggregate snapshot.
    durations: dict[tuple[str, str], Duration] = {}
    for line_no, raw_line in enumerate(lines[shutdown_index + 1 :], start=shutdown_index + 2):
        matched = DURATION_RE.match(clean_line(raw_line))
        if not matched:
            continue
        duration = Duration(
            group=matched.group("group"),
            label=matched.group("label"),
            samples=int(matched.group("samples")),
            total_us=int(matched.group("total_us")),
            max_us=int(matched.group("max_us")),
            line_no=line_no,
        )
        durations[(duration.group, duration.label)] = duration

    return shutdown_index + 1, list(durations.values())


def select_durations(
    durations: list[Duration], group: str, include_active: bool
) -> list[Duration]:
    """选出可放入同一饼图的非零项，并按耗时由大到小排序。"""

    selected = [
        duration
        for duration in durations
        if duration.group == group
        and duration.total_us > 0
        and (include_active or not duration.label.endswith("_active"))
    ]

    # Older kernels emitted both wall-clock syscall buckets and active buckets.
    # Prefer the active value for the default chart so old logs use the same
    # semantics as newer logs, where the historical label is active-only.
    if group == "syscall_duration" and not include_active:
        for label, active_label in (
            ("wait", "wait_active"),
            ("accept", "accept_active"),
            ("read", "read_active"),
        ):
            active_duration = next(
                (
                    duration
                    for duration in durations
                    if duration.group == group
                    and duration.label == active_label
                    and duration.total_us > 0
                ),
                None,
            )
            if active_duration is None:
                continue
            selected = [
                replace(
                    duration,
                    samples=active_duration.samples,
                    total_us=active_duration.total_us,
                    max_us=active_duration.max_us,
                )
                if duration.label == label
                else duration
                for duration in selected
            ]
    return sorted(selected, key=lambda duration: duration.total_us, reverse=True)


def format_duration(total_us: int) -> str:
    """将微秒转换为紧凑且保持精度的可读格式。"""

    if total_us < 1_000:
        return f"{total_us} us"
    if total_us < 1_000_000:
        return f"{total_us / 1_000:.3f} ms"
    return f"{total_us / 1_000_000:.3f} s"


def pie_path(cx: float, cy: float, radius: float, start: float, end: float) -> str:
    """构造顺时针 SVG 饼图扇区路径，角度单位为弧度。"""

    start_x = cx + radius * math.cos(start)
    start_y = cy + radius * math.sin(start)
    end_x = cx + radius * math.cos(end)
    end_y = cy + radius * math.sin(end)
    large_arc = 1 if end - start > math.pi else 0
    return (
        f"M {cx:.2f} {cy:.2f} L {start_x:.2f} {start_y:.2f} "
        f"A {radius:.2f} {radius:.2f} 0 {large_arc} 1 {end_x:.2f} {end_y:.2f} Z"
    )


def render_svg(
    durations: list[Duration],
    output_path: Path,
    title: str,
    source_name: str,
) -> int:
    """生成饼图和包含耗时、百分比的图例，并返回总耗时。"""

    total_us = sum(duration.total_us for duration in durations)
    legend_line_height = 34
    width = 1_500
    height = max(760, 175 + len(durations) * legend_line_height + 70)
    center_x = 335
    center_y = height / 2 + 25
    radius = min(250, center_y - 110)
    legend_x = 670
    legend_y = 155

    svg: list[str] = [
        '<?xml version="1.0" encoding="UTF-8"?>',
        (
            f'<svg xmlns="http://www.w3.org/2000/svg" width="{width}" '
            f'height="{height}" viewBox="0 0 {width} {height}">'
        ),
        "<style>",
        ".title { font: 700 27px sans-serif; fill: #0f172a; }",
        ".subtitle { font: 16px sans-serif; fill: #475569; }",
        ".legend { font: 17px sans-serif; fill: #1e293b; }",
        ".percent { font: 600 15px sans-serif; fill: #ffffff; }",
        ".border { stroke: #ffffff; stroke-width: 2; }",
        "</style>",
        f'<rect width="{width}" height="{height}" fill="#ffffff"/>',
        f'<text x="60" y="55" class="title">{html.escape(title)}</text>',
        (
            f'<text x="60" y="88" class="subtitle">source: '
            f'{html.escape(source_name)} | aggregate total: {format_duration(total_us)}</text>'
        ),
    ]

    angle = -math.pi / 2
    for index, duration in enumerate(durations):
        fraction = duration.total_us / total_us
        end_angle = angle + fraction * math.tau
        color = COLORS[index % len(COLORS)]
        if len(durations) == 1:
            svg.append(
                f'<circle cx="{center_x}" cy="{center_y}" r="{radius}" '
                f'fill="{color}" class="border"/>'
            )
        else:
            svg.append(
                f'<path d="{pie_path(center_x, center_y, radius, angle, end_angle)}" '
                f'fill="{color}" class="border"/>'
            )

        # Percentage labels are kept inside meaningful sectors only. The full
        # percentage is always present in the legend, including tiny sectors.
        if fraction >= 0.035:
            mid_angle = (angle + end_angle) / 2
            text_x = center_x + radius * 0.63 * math.cos(mid_angle)
            text_y = center_y + radius * 0.63 * math.sin(mid_angle) + 5
            svg.append(
                f'<text x="{text_x:.2f}" y="{text_y:.2f}" text-anchor="middle" '
                f'class="percent">{fraction * 100:.1f}%</text>'
            )

        text_y = legend_y + index * legend_line_height
        legend_text = (
            f"{duration.label}: {format_duration(duration.total_us)} "
            f"({fraction * 100:.2f}%, samples={duration.samples}, "
            f"max={format_duration(duration.max_us)})"
        )
        svg.extend(
            [
                (
                    f'<rect x="{legend_x}" y="{text_y - 15}" width="18" height="18" '
                    f'fill="{color}"/>'
                ),
                (
                    f'<text x="{legend_x + 30}" y="{text_y}" class="legend">'
                    f"{html.escape(legend_text)}</text>"
                ),
            ]
        )
        angle = end_angle

    svg.append("</svg>")
    try:
        output_path.parent.mkdir(parents=True, exist_ok=True)
        output_path.write_text("\n".join(svg) + "\n", encoding="utf-8")
    except OSError as error:
        raise ValueError(f"无法写入 {output_path}: {error}") from error
    return total_us


def default_output_path(log_path: Path, group: str) -> Path:
    """将 ``log.ans`` 映射为 ``log.<group>.svg``。"""

    return log_path.with_name(f"{log_path.stem}.{group}.svg")


def print_summary(durations: list[Duration], total_us: int) -> None:
    """在终端输出与 SVG 图例一致的文字统计。"""

    print(f"统计总耗时: {format_duration(total_us)}")
    for duration in durations:
        percentage = duration.total_us / total_us * 100
        print(
            f"  {duration.label:16} {format_duration(duration.total_us):>12} "
            f"{percentage:6.2f}%  samples={duration.samples} "
            f"max={format_duration(duration.max_us)}"
        )


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="统计 shutdown! 后的 Ya2yOS perf 耗时并生成 SVG 饼图"
    )
    parser.add_argument("logfile", type=Path, help="例如 log.ans")
    parser.add_argument(
        "--group",
        default="syscall_duration",
        help="要绘制的 perf 指标族，默认 syscall_duration",
    )
    parser.add_argument(
        "--include-active",
        action="store_true",
        help="保留 *_active 子指标；这会与其父项发生重复计时",
    )
    parser.add_argument(
        "--list-groups",
        action="store_true",
        help="列出最终快照中的可用指标族后退出",
    )
    parser.add_argument(
        "-o",
        "--output",
        type=Path,
        help="SVG 输出路径，默认与日志同目录",
    )
    parser.add_argument(
        "--title",
        help="图表标题，默认根据指标族生成",
    )
    return parser.parse_args()


def main() -> None:
    args = parse_args()
    try:
        shutdown_line, all_durations = parse_final_snapshot(args.logfile)
    except ValueError as error:
        print(f"error: {error}", file=sys.stderr)
        raise SystemExit(1) from error

    groups = sorted({duration.group for duration in all_durations})
    if args.list_groups:
        print(f"最后一个 shutdown! 位于第 {shutdown_line} 行。可用指标族：")
        for group in groups:
            count = sum(duration.group == group for duration in all_durations)
            print(f"  {group} ({count} 项)")
        return

    selected = select_durations(all_durations, args.group, args.include_active)
    if not selected:
        available = ", ".join(groups) or "无"
        print(
            f"error: 最终快照中没有可绘制的 {args.group} 指标。可用指标族: {available}",
            file=sys.stderr,
        )
        raise SystemExit(1)

    output_path = args.output or default_output_path(args.logfile, args.group)
    title = args.title or f"Ya2yOS final {args.group} totals"
    try:
        total_us = render_svg(selected, output_path, title, args.logfile.name)
    except ValueError as error:
        print(f"error: {error}", file=sys.stderr)
        raise SystemExit(1) from error

    print(f"解析最后一个 shutdown!（第 {shutdown_line} 行）的 {args.group}。")
    if not args.include_active:
        print(
            "已排除 *_active 嵌套子指标；旧日志中的 wait/accept/read 优先采用对应的 active 指标。"
            "使用 --include-active 可将其加入。"
        )
    if args.group != "syscall_duration":
        print(
            "注意：该指标族可能包含父子嵌套阶段；图中比例是所选记录项的累计耗时占比，"
            "不代表互斥的实际墙钟时间。"
        )
    print_summary(selected, total_us)
    print(f"SVG 饼图已写入: {output_path}")


if __name__ == "__main__":
    try:
        main()
    except BrokenPipeError:
        with open(os.devnull, "w", encoding="utf-8") as devnull:
            os.dup2(devnull.fileno(), sys.stdout.fileno())
