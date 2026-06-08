#!/usr/bin/env python3
"""
Kernel log analyzer for PlainOs.

Parses log lines of the form:
    [LEVEL] [HART{id}] [PID {pid}] [TID {tid}] {message}

Usage
-----
  # 默认模式：只看 WARN/ERROR + LoadPageFault + panic，去重 + 摘要
  python3 analyze_logs.py log.ans

  # 只看系统调用序列（折叠重复调用）
  python3 analyze_logs.py log.ans -s

  # 系统调用序列 + 不折叠（完整输出）
  python3 analyze_logs.py log.ans -s --no-dedup | less

  # 调试：显示所有 DEBUG 及以上，带 3 行上下文
  python3 analyze_logs.py log.ans -l DEBUG -c 3 | less

  # 只显示最近 N 条 WARN/ERROR
  python3 analyze_logs.py log.ans -l ERROR

Flags
-----
  -l, --level    最小日志级别: TRACE/DEBUG/INFO/WARN/ERROR (默认: WARN)
  -s, --syscalls 系统调用追踪模式，只显示 [syscall begin]/[syscall ret]
  -c, --context  每条高亮行前打印 N 行上下文 (默认: 0)
  --no-dedup     关闭连续重复折叠
  --no-summary   关闭末尾统计摘要
"""
import re
import sys
import argparse
from collections import Counter, defaultdict


# Patterns
ANSI_ESCAPE = re.compile(r"\x1b\[[0-9;]*m")

# Full line structure:  [LEVEL] [HART\d+] [PID -?\d+] [TID -?\d+] <rest>
LINE_RE = re.compile(
    r"\[(?P<level>\w+)\]\s+"
    r"\[HART(?P<hart>\d+)\]\s+"
    r"\[PID\s+(?P<pid>-?\d+)\]\s+"
    r"\[TID\s+(?P<tid>-?\d+)\]\s+"
    r"(?P<msg>.*)"
)

# Sub-patterns inside the message body
SYSCALL_BEGIN_RE = re.compile(r"\[syscall begin\]\s+(\w+)")
SYSCALL_RET_RE = re.compile(r"\[syscall ret --- (OK|ERR)\]\s*(.*)")
TRAP_RE = re.compile(r"\[trap_handler\]:\s*scause=(\w+)\((\w+)\)")
RETURN_TO_USER_RE = re.compile(r"\[return_to_user\]")

# Notable message keywords (always highlighted)
# Avoid short substrings like "FAIL" or "timeout" that cause false positives
HIGHLIGHT_KEYWORDS = [
    "panic", "PANIC",
    "IllegalInstruction", "Breakpoint", "Misaligned",
    "killed by signal", "Segfault", "abort",
    "oom", "OOM", "Out of memory", "no memory",
    "BUG", "assertion failed",
]

# Exceptions worth highlighting even at WARN level (unusual / memory-related)
# StorePageFault is normal in COW/lazy-alloc kernels – only flag at DEBUG+.
INTERESTING_EXCEPTIONS = {
    "LoadPageFault", "PageFault",              # access to unmapped memory
    "IllegalInstruction", "Breakpoint", "Misaligned",
    "AccessFault",
}

# Page faults that are normal in a lazy/COW kernel – show only at DEBUG+
NORMAL_PAGE_FAULTS = {"StorePageFault"}

# Exceptions that are normal / routine (only shown at DEBUG level)
ROUTINE_EXCEPTIONS = {"Syscall", "Timer"}

# Core logic
def analyze_log(file_path, min_level="WARN", context=0, dedup=True,
                summary=True, syscalls=False):
    """Parse and print important log messages."""

    LEVEL_RANK = {"TRACE": 0, "DEBUG": 1, "INFO": 2, "WARN": 3, "ERROR": 4}
    min_rank = LEVEL_RANK.get(min_level.upper(), 2)

    # State
    pid_syscall: dict[int, str] = {}            # pid -> last syscall name
    syscall_counts: Counter = Counter()
    level_counts: Counter = Counter()
    trap_counts: Counter = Counter()
    error_lines: list[tuple[int, str]] = []      # (line_no, text) for summary

    ring_buffer: list[str] = []                  # for context window
    last_printed: str | None = None              # for dedup
    line_no = 0
    last_syscall_line: tuple[int, str] | None = None  # for pairing begin/ret
    # Syscall-mode dedup: collapse repeated identical begin-ret pairs
    # sc_pending holds the last completed call: (pid, sc_name, ret_str, count)
    sc_pending: tuple | None = None
    sc_pending_count = 0
    _last_warn_key: tuple | None = None    # dedup consecutive identical warnings
    _last_warn_count = 0

    def _flush_sc_pending():
        nonlocal sc_pending, sc_pending_count
        if sc_pending_count > 1 and sc_pending:
            extra = sc_pending_count - 1
            plural = "" if extra == 1 else "s"
            print(f"        \033[90m... repeated {extra} more time{plural}\033[0m")
        sc_pending = None
        sc_pending_count = 0

    def _flush_warn_pending():
        nonlocal _last_warn_key, _last_warn_count
        if _last_warn_count > 1 and _last_warn_key:
            extra = _last_warn_count - 1
            plural = "" if extra == 1 else "s"
            print(f"        \033[90m... repeated {extra} more time{plural}\033[0m")
        _last_warn_key = None
        _last_warn_count = 0

    try:
        with open(file_path, "r", errors="replace") as f:
            for raw_line in f:
                line_no += 1
                clean = ANSI_ESCAPE.sub("", raw_line).strip()
                if not clean:
                    continue

                m = LINE_RE.match(clean)
                if not m:
                    # Non-kernel-log lines (e.g. OpenSBI banner, QEMU output)
                    ring_buffer.append(clean)
                    if len(ring_buffer) > context:
                        ring_buffer.pop(0)
                    continue

                level = m.group("level")
                pid = int(m.group("pid"))
                msg = m.group("msg")
                rank = LEVEL_RANK.get(level, 0)

                level_counts[level] += 1

                ring_buffer.append(clean)
                if len(ring_buffer) > context:
                    ring_buffer.pop(0)

                # --- Track WARN/ERROR for summary ---
                if rank >= 3:
                    error_lines.append((line_no, clean))

                # --- Update syscall context ---
                s_begin = SYSCALL_BEGIN_RE.search(msg)
                s_ret = SYSCALL_RET_RE.search(msg)

                if s_begin:
                    sc_name = s_begin.group(1)
                    pid_syscall[pid] = sc_name
                    syscall_counts[sc_name] += 1

                if s_ret and s_ret.group(1) == "ERR":
                    error_lines.append((line_no, clean))

                # --- Track traps ---
                t = TRAP_RE.search(msg)
                if t:
                    trap_counts[f"{t.group(1)}({t.group(2)})"] += 1

                # ========================================================
                #  Syscall trace mode: compact begin/ret paired output
                # ========================================================
                if syscalls:
                    printed = False

                    if s_begin:
                        sc_name = s_begin.group(1)
                        _sc_cur_begin = (pid, sc_name)

                    elif s_ret:
                        status = s_ret.group(1)
                        rest = s_ret.group(2).strip()
                        sc_name = pid_syscall.get(pid, "?")
                        ret_key = (pid, sc_name, rest)

                        if dedup and sc_pending and sc_pending[:3] == ret_key:
                            sc_pending_count += 1
                        else:
                            _flush_sc_pending()
                            _flush_warn_pending()
                            sc_pending = ret_key
                            sc_pending_count = 1
                            color = "\033[32m" if status == "OK" else "\033[31m"
                            print(f"  L{line_no:>6d} [PID {pid:>3}] "
                                  f"\033[36m{sc_name:20s}\033[0m "
                                  f"{color}=> {status}  {rest}\033[0m")
                        printed = True

                    # Also show WARN/ERROR/panic lines in syscall mode —
                    # they reveal what went wrong and what syscall was active.
                    # Don't flush the syscall batch — warnings interleaved with
                    # the same syscall are still one logical batch.
                    if rank >= 3 or "panic" in msg.lower():
                        syscall_ctx = pid_syscall.get(pid, "")
                        label = "PANIC!" if "panic" in msg.lower() else level
                        # Dedup on message content (exclude line number)
                        warn_key = (pid, label, msg)
                        if not dedup or warn_key != _last_warn_key:
                            _flush_warn_pending()
                            _last_warn_key = warn_key
                            _last_warn_count = 1
                            ctx_str = f" [in {syscall_ctx}]" if syscall_ctx else ""
                            print(f"  L{line_no:>6d} [PID {pid:>3}] "
                                  f"\033[1;33m{label}\033[0m{ctx_str}: {msg}")
                        else:
                            _last_warn_count += 1
                        printed = True

                    if printed:
                        continue
                    # Skip everything else (DEBUG traps, file ops, etc.)
                    continue

                # ========================================================
                #  Normal mode: severity + exception based filtering
                # ========================================================

                is_panic = "panic" in msg.lower()
                is_failed_syscall = "[syscall ret --- ERR]" in msg
                has_keyword = any(kw in msg for kw in HIGHLIGHT_KEYWORDS)

                # Trap analysis
                trap_m = TRAP_RE.search(msg)
                trap_type = "" if not trap_m else trap_m.group(2)
                is_interesting_trap = trap_type in INTERESTING_EXCEPTIONS
                is_routine_trap = trap_type in ROUTINE_EXCEPTIONS
                is_normal_page_fault = trap_type in NORMAL_PAGE_FAULTS

                # Priority-ordered highlight logic:
                #   Priority 1 (always): ERROR/WARN lines, panics, failed syscalls,
                #       keyword hits, interesting traps (LoadPageFault etc.)
                #   Priority 2 (DEBUG+): normal page faults (StorePageFault),
                #       routine traps (Syscall, Timer)
                #   Priority 3 (DEBUG+): any line meeting --level threshold
                if rank >= 3:                             # ERROR or WARN
                    highlight = True
                elif is_panic or is_failed_syscall or has_keyword or is_interesting_trap:
                    highlight = True                      # always show
                elif (is_routine_trap or is_normal_page_fault) and min_rank <= 1:
                    highlight = True                      # DEBUG/TRACE only
                elif rank >= min_rank and min_rank <= 1:  # DEBUG/TRACE: show all
                    highlight = True
                else:
                    highlight = False

                if not highlight:
                    continue

                # --- Dedup ---
                if dedup and clean == last_printed:
                    continue
                last_printed = clean

                # --- Context ---
                if context > 0:
                    for ctx_line in ring_buffer[:-1]:
                        print(f"  | {ctx_line}")

                # --- Print ---
                prefix = _prefix_for_line(level, is_panic, is_failed_syscall)
                syscall_ctx = pid_syscall.get(pid, "")
                ctx_str = f" [in {syscall_ctx}]" if syscall_ctx else ""
                print(f"{prefix} L{line_no}{ctx_str}: {msg}")

        # --- Flush any pending syscall / warn repeats ---
        if syscalls:
            _flush_sc_pending()
            _flush_warn_pending()

        # --- Summary ---
        if summary:
            print("\n" + "=" * 60)
            print(" SUMMARY")
            print("=" * 60)
            print(f"  Total lines parsed : {line_no}")
            print(f"  Log level counts   : {dict(level_counts)}")
            print(f"  Errors/Warnings    : {len(error_lines)}")
            if syscall_counts:
                print(f"  Syscall counts (top 15):")
                for sc, cnt in syscall_counts.most_common(15):
                    print(f"    {sc:25s} {cnt:6d}")
            if trap_counts:
                print(f"  Trap/Exception counts:")
                for tr, cnt in trap_counts.most_common():
                    print(f"    {tr:30s} {cnt:6d}")
            if error_lines:
                print(f"  Last 10 error/warning/pnic lines:")
                for ln, text in error_lines[-10:]:
                    print(f"    L{ln:6d}: {text[:120]}")

    except FileNotFoundError:
        print(f"error: file not found: {file_path}", file=sys.stderr)
        sys.exit(1)


def _prefix_for_line(level: str, is_panic: bool, is_failed: bool) -> str:
    if is_panic:
        return "\033[1;31m[PANIC!]\033[0m"
    if is_failed:
        return "\033[1;33m[FAILED]\033[0m"
    if level == "ERROR":
        return "\033[31m[ERROR]\033[0m"
    if level == "WARN":
        return "\033[33m[WARN]\033[0m"
    return f"[{level}]"


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------

def main():
    parser = argparse.ArgumentParser(
        description="Analyze PlainOs kernel log (log.ans)",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog="Run with no arguments or see script header for detailed usage.",
    )
    parser.add_argument("file", help="Path to log file (e.g. log.ans)")
    parser.add_argument(
        "-l", "--level", default="WARN",
        choices=["TRACE", "DEBUG", "INFO", "WARN", "ERROR"],
        help="Minimum log level to highlight (default: WARN)",
    )
    parser.add_argument(
        "-c", "--context", type=int, default=0,
        help="Number of context lines to print before each highlighted line",
    )
    parser.add_argument(
        "--no-dedup", action="store_true",
        help="Disable consecutive duplicate suppression",
    )
    parser.add_argument(
        "--no-summary", action="store_true",
        help="Disable summary at end",
    )
    parser.add_argument(
        "-s", "--syscalls", action="store_true",
        help="Syscall trace mode: only show [syscall begin] and [syscall ret]",
    )
    args = parser.parse_args()

    analyze_log(
        file_path=args.file,
        min_level=args.level,
        context=args.context,
        dedup=not args.no_dedup,
        summary=not args.no_summary,
        syscalls=args.syscalls,
    )


if __name__ == "__main__":
    main()