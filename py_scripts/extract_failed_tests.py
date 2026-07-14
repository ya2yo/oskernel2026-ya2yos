#!/usr/bin/env python3
"""从 Ya2yOS 测试日志中提取实际失败的测试。

支持 ``loongarch.ans`` 中的两类失败格式：

* libctest: ``FAIL <name> [status ...]``；
* LTP: ``TFAIL`` / ``TBROK``，或 ``FAIL LTP CASE <name> : <non-zero>``。

``FAIL LTP CASE <name> : 0`` 是当前 LTP 包装器固定打印的 wait status，
不是失败依据，因此会被忽略。

Usage:
    python3 py_scripts/extract_failed_tests.py loongarch.ans
    python3 py_scripts/extract_failed_tests.py --details loongarch.ans
"""

from __future__ import annotations

import argparse
import os
import re
import sys
from dataclasses import dataclass, field


ANSI_ESCAPE = re.compile(r"\x1b\[[0-?]*[ -/]*[@-~]")
GROUP_START_RE = re.compile(r"^#### OS COMP TEST GROUP START (.+) ####$")
LIBCTEST_START_RE = re.compile(
    r"^========== START (entry-(?:static|dynamic)\.exe) (.+) ==========$"
)
LIBCTEST_FAIL_RE = re.compile(r"^FAIL ([^\s]+) \[(.+)]$")
LTP_START_RE = re.compile(r"^RUN LTP CASE (.+)$")
LTP_TAG_RE = re.compile(r"\b(TFAIL|TBROK)\b\s*:")
LTP_RESULT_RE = re.compile(
    r"^(?:FAIL LTP CASE|RESULT GLIBC LTP SINGLE CASE) (.+) : (-?\d+)$"
)
DETAIL_RE = re.compile(
    r"\b(?:failed|failure|error|panic|segmentation fault|timed out|"
    r"assert(?:ion)?|abort(?:ed)?)\b",
    re.IGNORECASE,
)


@dataclass
class FailedTest:
    group: str
    name: str
    first_line: int
    reasons: list[str] = field(default_factory=list)
    details: list[tuple[int, str]] = field(default_factory=list)


def clean_line(raw_line: str) -> str:
    """删除 QEMU 日志中的 ANSI 控制符、NUL 和行尾空白。"""
    return ANSI_ESCAPE.sub("", raw_line).replace("\0", "").strip()


def add_failure(
    failures: dict[tuple[str, str], FailedTest],
    group: str,
    name: str,
    line_no: int,
    reason: str,
    details: list[tuple[int, str]],
) -> None:
    key = (group, name)
    failure = failures.setdefault(key, FailedTest(group, name, line_no))
    if reason not in failure.reasons:
        failure.reasons.append(reason)
    for detail in details:
        if detail not in failure.details:
            failure.details.append(detail)


def extract_failed_tests(file_path: str) -> list[FailedTest]:
    """解析日志并返回失败测试，保持它们在日志中的首次出现顺序。"""
    failures: dict[tuple[str, str], FailedTest] = {}
    group = "unknown"
    current_name: str | None = None
    current_kind: str | None = None
    current_details: list[tuple[int, str]] = []

    with open(file_path, "r", encoding="utf-8", errors="replace") as log_file:
        for line_no, raw_line in enumerate(log_file, start=1):
            line = clean_line(raw_line)

            group_match = GROUP_START_RE.match(line)
            if group_match:
                group = group_match.group(1)
                current_name = None
                current_kind = None
                current_details = []
                continue

            libc_start = LIBCTEST_START_RE.match(line)
            if libc_start:
                current_kind = "libctest"
                current_name = f"{libc_start.group(1)} {libc_start.group(2)}"
                current_details = []
                continue

            ltp_start = LTP_START_RE.match(line)
            if ltp_start:
                current_kind = "ltp"
                current_name = ltp_start.group(1)
                current_details = []
                continue

            if current_name and DETAIL_RE.search(line):
                current_details.append((line_no, line))

            libc_fail = LIBCTEST_FAIL_RE.match(line)
            if libc_fail and not line.startswith("FAIL LTP CASE "):
                variant = current_name.split(" ", 1)[0] if current_name else "unknown"
                name = f"{variant} {libc_fail.group(1)}"
                add_failure(
                    failures,
                    group,
                    name,
                    line_no,
                    libc_fail.group(2),
                    current_details,
                )
                continue

            ltp_tag = LTP_TAG_RE.search(line)
            if ltp_tag:
                name = current_name or "unknown LTP test"
                add_failure(
                    failures,
                    group,
                    name,
                    line_no,
                    ltp_tag.group(1),
                    current_details + [(line_no, line)],
                )
                continue

            ltp_result = LTP_RESULT_RE.match(line)
            if ltp_result:
                wait_status = int(ltp_result.group(2))
                if wait_status != 0:
                    add_failure(
                        failures,
                        group,
                        ltp_result.group(1),
                        line_no,
                        f"wait status {wait_status}",
                        current_details,
                    )

    return sorted(failures.values(), key=lambda failure: failure.first_line)


def print_failures(failures: list[FailedTest], show_details: bool) -> None:
    if not failures:
        print("未发现失败测试。")
        return

    print(f"发现 {len(failures)} 个失败测试：")
    for failure in failures:
        reasons = ", ".join(failure.reasons)
        print(f"L{failure.first_line}: [{failure.group}] {failure.name} ({reasons})")
        if show_details:
            for line_no, detail in failure.details:
                print(f"  L{line_no}: {detail}")


def main() -> None:
    parser = argparse.ArgumentParser(description="提取 Ya2yOS 日志中的失败测试")
    parser.add_argument("logfile", help="例如 loongarch.ans")
    parser.add_argument(
        "--details",
        action="store_true",
        help="同时显示每个失败测试区间中的诊断行",
    )
    args = parser.parse_args()

    try:
        print_failures(extract_failed_tests(args.logfile), args.details)
        sys.stdout.flush()
    except BrokenPipeError:
        raise
    except OSError as error:
        parser.error(f"无法读取 {args.logfile}: {error}")


if __name__ == "__main__":
    try:
        main()
    except BrokenPipeError:
        # Allow normal shell use such as ``... | head`` without a traceback.
        with open(os.devnull, "w", encoding="utf-8") as devnull:
            os.dup2(devnull.fileno(), sys.stdout.fileno())
