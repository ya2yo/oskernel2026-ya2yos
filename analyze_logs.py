#!/bin/python
import re
import sys

def analyze_log(file_path):
    # match [syscall begin]
    syscall_pattern = re.compile(r"\[syscall begin\]\s+(\w+)")
    warn_pattern = re.compile(r"\[WARN\].*\[PID (d+)\].*")
    pid_context = {}
    last_seen_syscall = "unknown"
    try:
        with open(file_path, 'r') as f:
            for line in f:
                # delete ANSI color char
                clean_line = re.sub(r'\x1b\[[0-9;]*m', '', line).strip()
                # try to match the start of syscall
                syscall_match = syscall_pattern.search(clean_line)
                if syscall_match:
                    last_seen_syscall = syscall_match.group(1)
                    continue
                # try to match warning message
                warn_match = warn_pattern.search(clean_line)
                if warn_match:
                    pid = warn_match.group(1)
                    file_path_in_log = warn_match.group(2)

                    # print the message
                    print(f"[pid {pid}] {last_seen_syscall}")
    except FileNotFoundError:
        print(f"error: file not found: {file_path}")
if __name__ == "_main_":
    if len(sys.argv) < 2:
        print("Usage: python3 analyze_logs.py <log_file>")
    else:
        analyze_log(sys.argv[1])