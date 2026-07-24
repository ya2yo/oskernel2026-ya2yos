# AGENTS.md

## Project

This repository is Ya2yOS, an OS kernel project for the OS competition. It is
based on TatlinOS and contains kernel code, user programs, contest documents,
and shared agent skills.

Use Chinese when discussing project status, debugging conclusions, and final
summaries with the maintainer unless they ask otherwise. Keep code identifiers,
commands, paths, and errors in their original language.

## Repository Layout

- `os/`: kernel implementation.
- `user/`: user-space programs and test entry points.
- `crates/`: shared local Rust crates, including the customized `smoltcp`.
- `scripts/`: architecture-specific build and QEMU settings, plus repository
  maintenance and test helper scripts.
- `Docs/`: project documentation, development logs, and problem writeups.
- `Docs/决赛文档/problem/`: detailed bug and testcase analysis records.
- `agent-skills/shared/`: source of truth for repo-specific agent skills.
- `.codex/skills/` and `.claude/skills/`: generated skill directories. Do not
  edit generated skill files directly.
- `~/projects/OSKernel2026-PlainOs/testsuits-for-oskernel` the testcase source code, it's read only and not compile it.

## Agent Skills

Use the repo skills when the task matches their scope:

- `add-syscall-feature`: new kernel features, new Linux syscalls, syscall number
  wiring, or syscall semantic changes.
- `fix-bug`: panic, LTP failure, blocking/hang, semantic bug, log analysis, or
  architecture-specific failure.
- `write-docs`: development log entries, `problem/` writeups, `ai.log`, or
  `AI_INTERACTION.md` updates.

When changing skills, edit only `agent-skills/shared/`, then run:

```bash
python3 scripts/sync_agent_skills.py
python3 scripts/sync_agent_skills.py --check
```

## Build And Run

The default architecture in the root `Makefile` may change during development.
Use explicit `TARGET_ARCH` values when architecture matters.

Common commands:

```bash
make
make TARGET_ARCH=riscv64
make TARGET_ARCH=loongarch64
make log
make run
```

Notes:

- `make` builds with the current/default architecture.
- `make log` builds with debug logging enabled.
- `make run` creates a temporary `disk.img` symlink, runs QEMU, then removes the
  symlink.
- The build copies `os/dotcargo` and `user/dotcargo` to temporary `.cargo`
  directories and removes them afterward; this is expected.

## Validation Expectations

- For small docs-only changes, do not run kernel builds unless needed.
- For kernel or user-space code changes, run at least `make` when feasible.
- For behavior changes, bug fixes, syscall changes, scheduler/signal/mm/fs/net
  changes, or anything touching test execution, also run the relevant `make log`
  or `make run` path.
- For architecture-sensitive changes, state which of `riscv64` and
  `loongarch64` were validated. Run both when practical.
- When reading LTP output, prefer `TPASS`, `TFAIL`, `TBROK`, panic messages, and
  summary lines. Do not rely only on wrapper lines such as
  `FAIL LTP CASE ... : 0`.
- Put long temporary debug logs under `/tmp/` unless the maintainer asks for a
  workspace file.

Useful log filters:

```bash
rg -a -n "TPASS|TFAIL|TBROK|panic|ERROR|WARN|Summary" log.ans
strings log.ans | tail -80
```

## Editing Rules

- Do not revert user changes unless the maintainer explicitly asks.
- Keep fixes scoped to the requested feature, bug, or document update.
- Prefer `rg` / `rg --files` for search.
- Use structured parsers or existing project APIs when available.
- Use `apply_patch` for manual source edits.
- Avoid broad formatting churn and unrelated refactors.
- Do not edit vendored dependencies unless the task explicitly requires it.
- Do not manually edit generated `.codex/skills/` or `.claude/skills/` files.

## Kernel Implementation Guidelines

- Keep syscall handlers thin: argument decoding, fd lookup, user-memory copies,
  and delegation belong in `os/src/syscall/`; core semantics belong in `fs`,
  `task`, `mm`, `net`, architecture, or driver modules.
- Use Linux syscall numbers and preserve Linux-compatible semantics when adding
  or fixing syscalls.
- Access user pointers through `copy_from_user` / `copy_to_user` style helpers
  and check null pointers and lengths before copying.
- Propagate internal errors with `?` where appropriate. Do not swallow an
  `Err` and return `Ok(0)` unless that is the intended Linux-visible behavior.
- Avoid holding locks across blocking points. Keep lock ordering simple and
  consistent, especially in task, signal, futex, filesystem, and network paths.
- Treat scheduler, signal, page fault/COW, fd table, mount/fs, socket, and timer
  paths as high-risk shared behavior; validate narrowly and then broadly.

## Documentation Rules

- Project documentation lives under `Docs/`.
- Use Typst for externally facing, long-lived, or research/engineering design
  documents (for example, kernel designs, architecture proposals, technical
  reports, and presentation manuscripts). Commit the `.typ` source with one
  `main.typ` entry point and a reproducible PDF build command. Use Markdown for
  READMEs, development logs, problem writeups, lightweight indexes, and
  collaboration notes; do not use it as the main body of a formal design report.
- A formal Typst document must state its version, applicable source snapshot,
  intended readers, and implemented-versus-planned boundaries. Use accessible
  native Typst diagrams or traceable assets, and cite external material. PDFs
  are build artifacts and should not be committed unless the maintainer asks.
- Use `Docs/决赛文档/开发日志.md` for short chronological development notes.
- Use one file per issue in `Docs/决赛文档/problem/` for non-trivial bugs,
  testcase fixes, panic analysis, or syscall behavior investigations.
- Update `Docs/决赛文档/README.md` when adding a new problem writeup.
- When AI assistance is used for a substantive code/debugging change, update
  both `Docs/决赛文档/ai.log` and `Docs/决赛文档/AI_INTERACTION.md`, following the
  existing style.
- For trivial typo, formatting, or project-guidance-only edits, keep the final
  response clear about what was changed and avoid unnecessary log noise unless
  the maintainer asks for full AI records.

## Final Response

When finishing a task, report:

- Changed files.
- Root cause and fix, for bugs.
- Important implementation choices, for features.
- Commands run and their results.
- Validation not run, with the reason.
