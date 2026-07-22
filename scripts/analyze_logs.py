#!/usr/bin/env python3
"""
Kernel log analyzer for PlainOs.

Extracts two kinds of information from the log:
  1. System call sequences (begin -> ret pairs)
  2. LTP test output messages (e.g. tst_device.c:100: TINFO: ...)

Usage:
  python3 scripts/analyze_logs.py log.ans
  python3 scripts/analyze_logs.py log.ans | less
"""

import re
import sys


# -- Patterns ------------------------------------------------------------

ANSI_ESCAPE = re.compile(r"\x1b\[[0-9;]*m")

# Kernel log line: [LEVEL] [HART\d+] [PID -?\d+] [TID -?\d+] <message>
LINE_RE = re.compile(
    r"\[(?P<level>\w+)\]\s+"
    r"\[HART(?P<hart>\d+)\]\s+"
    r"\[PID\s+(?P<pid>-?\d+)\]\s+"
    r"\[TID\s+(?P<tid>-?\d+)\]\s+"
    r"(?P<msg>.*)"
)

# Syscall begin / ret inside the message body
SYSCALL_BEGIN_RE = re.compile(r"\[syscall begin\]\s+(\w+)")
SYSCALL_RET_RE = re.compile(r"\[syscall ret --- (OK|Err)\]\s*(.*)")

# LTP test output lines, e.g. tst_device.c:100: TINFO: Couldn't find free loop device
# Also catches other framework tags: TBROK, TPASS, TFAIL, TWARN, TCONF
LTP_TEST_RE = re.compile(
    r"^(.+?\.[ch](?:pp)?):(\d+):\s+(TINFO|TPASS|TFAIL|TBROK|TWARN|TCONF)\s*:\s*(.*)$"
)


# -- Core logic ----------------------------------------------------------

def analyze_log(file_path):
    """Parse the log file and print syscall sequences + LTP test output."""

    pid_syscall: dict[int, str] = {}   # pid -> pending syscall name from last begin
    pid_user_out: dict[int, list[str]] = {}  # pid -> accumulated user-mode output
    user_mode = False  # True once we see the first syscall (boot phase over)

    def _show_user_out(pid: int) -> str:
        """Return the reconstructed user-mode output for *pid* as an
        escaped string, or '' if there is none."""
        parts = pid_user_out.get(pid)
        if not parts:
            return ""
        text = "".join(parts)
        # Escape actual control characters for safe terminal display.
        out = []
        for ch in text:
            if ch == "\n":
                out.append("\\n")
            elif ch == "\r":
                out.append("\\r")
            elif ch == "\t":
                out.append("\\t")
            elif ch == "\0":
                out.append("\\0")
            elif ord(ch) < 32 or ord(ch) == 127:
                out.append(f"\\x{ord(ch):02x}")
            else:
                out.append(ch)
        return "".join(out)

    try:
        with open(file_path, "r", errors="replace") as f:
            for line_no, raw_line in enumerate(f, start=1):
                clean = ANSI_ESCAPE.sub("", raw_line).rstrip("\n").strip()
                if not clean:
                    continue

                # -- Try kernel log line --
                m = LINE_RE.search(clean)
                if m:
                    raw_prefix = clean[:m.start()]  # user output before [LEVEL]
                    pid = int(m.group("pid"))
                    msg = m.group("msg")

                    # Track syscall begin
                    s_begin = SYSCALL_BEGIN_RE.search(msg)
                    if s_begin:
                        sc_name = s_begin.group(1)
                        pid_syscall[pid] = sc_name
                        # Clear accumulated user output for each new syscall
                        pid_user_out.pop(pid, None)
                        user_mode = True
                        continue

                    # Print syscall ret
                    s_ret = SYSCALL_RET_RE.search(msg)
                    if s_ret:
                        user_mode = True
                        status = s_ret.group(1)
                        rest = s_ret.group(2).strip()
                        sc_name = pid_syscall.pop(pid, "?")
                        color = "\033[32m" if status == "OK" else "\033[31m"

                        if raw_prefix:
                            pid_user_out.setdefault(pid, []).append(raw_prefix)

                        user_str = _show_user_out(pid)

                        line = (f"  L{line_no:>6d} [PID {pid:>3}] "
                                f"\033[36m{sc_name:20s}\033[0m "
                                f"{color}=> {status}  {rest}\033[0m")
                        if user_str:
                            line += f"  \033[37m|\033[0m \033[37m{user_str}\033[0m"
                        print(line)
                        continue

                    # Non-syscall kernel lines (DEBUG, INFO, etc.) – skip
                    continue

                # -- Try LTP test output line --
                t = LTP_TEST_RE.match(clean)
                if t:
                    filename = t.group(1)
                    lineno = t.group(2)
                    tag = t.group(3)
                    message = t.group(4)
                    tag_colors = {
                        "TPASS": "\033[32m",
                        "TFAIL": "\033[31m",
                        "TBROK": "\033[31m",
                        "TINFO": "\033[36m",
                        "TWARN": "\033[33m",
                        "TCONF": "\033[33m",
                    }
                    c = tag_colors.get(tag, "\033[0m")
                    print(f"  L{line_no:>6d} {filename}:{lineno}: "
                          f"{c}{tag}\033[0m: {message}")
                    continue

                # -- Unmatched line --
                if user_mode:
                    # After first syscall, unmatched lines are user-mode output
                    print(f"  L{line_no:>6d} \033[37m{clean}\033[0m")
                # else: boot messages before first syscall – silently skip

    except FileNotFoundError:
        print(f"error: file not found: {file_path}", file=sys.stderr)
        sys.exit(1)


# -- CLI -----------------------------------------------------------------

def main():
    if len(sys.argv) < 2:
        print("usage: python3 scripts/analyze_logs.py <logfile>", file=sys.stderr)
        sys.exit(1)

    analyze_log(file_path=sys.argv[1])


if __name__ == "__main__":
    main()
