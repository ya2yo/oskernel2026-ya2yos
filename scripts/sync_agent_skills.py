#!/usr/bin/env python3
"""Sync shared agent skills into Codex and Claude Code skill directories."""

from __future__ import annotations

import argparse
import filecmp
import shutil
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
SHARED = ROOT / ".agents" / "skills"
TARGETS = (ROOT / ".codex" / "skills", ROOT / ".claude" / "skills", ROOT / ".opencode" / "skills")


def copytree_replace(src: Path, dst: Path) -> None:
    if dst.exists():
        shutil.rmtree(dst)
    shutil.copytree(
        src,
        dst,
        ignore=shutil.ignore_patterns("__pycache__", ".DS_Store"),
    )


def copy_file_replace(src: Path, dst: Path) -> None:
    dst.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(src, dst)


def shared_skill_dirs() -> list[Path]:
    if not SHARED.exists():
        return []
    return sorted(path for path in SHARED.iterdir() if (path / "SKILL.md").is_file())


def root_files(base: Path) -> list[Path]:
    if not base.exists():
        return []
    return sorted(path for path in base.iterdir() if path.is_file())


def init_shared(source: Path, force: bool) -> None:
    source = source.resolve()
    if not source.is_dir():
        raise SystemExit(f"source skill directory does not exist: {source}")

    if SHARED.exists() and any(SHARED.iterdir()) and not force:
        raise SystemExit(
            f"{SHARED} is not empty; pass --force-init to replace it from {source}"
        )

    if SHARED.exists() and force:
        shutil.rmtree(SHARED)
    SHARED.mkdir(parents=True, exist_ok=True)

    for item in sorted(source.iterdir()):
        if item.is_dir() and (item / "SKILL.md").is_file():
            copytree_replace(item, SHARED / item.name)
        elif item.is_file():
            copy_file_replace(item, SHARED / item.name)


def sync_targets() -> None:
    skills = shared_skill_dirs()
    if not skills:
        raise SystemExit(f"no skills found in {SHARED}; seed it with --init-from first")

    expected_roots = {path.name for path in root_files(SHARED)}
    expected_skills = {path.name for path in skills}

    for target in TARGETS:
        target.mkdir(parents=True, exist_ok=True)
        for item in sorted(target.iterdir()):
            if item.is_dir() and (item / "SKILL.md").is_file() and item.name not in expected_skills:
                shutil.rmtree(item)
            elif item.is_file() and item.name not in expected_roots:
                item.unlink()
        for item in root_files(SHARED):
            copy_file_replace(item, target / item.name)
        for skill in skills:
            copytree_replace(skill, target / skill.name)


def compare_dirs(left: Path, right: Path, rel: Path = Path()) -> list[str]:
    problems: list[str] = []
    left_names = {path.name for path in left.iterdir()} if left.exists() else set()
    right_names = {path.name for path in right.iterdir()} if right.exists() else set()

    for name in sorted(left_names - right_names):
        problems.append(f"missing in {right}: {rel / name}")
    for name in sorted(right_names - left_names):
        problems.append(f"extra in {right}: {rel / name}")

    for name in sorted(left_names & right_names):
        left_path = left / name
        right_path = right / name
        child_rel = rel / name
        if left_path.is_dir() and right_path.is_dir():
            problems.extend(compare_dirs(left_path, right_path, child_rel))
        elif left_path.is_file() and right_path.is_file():
            if not filecmp.cmp(left_path, right_path, shallow=False):
                problems.append(f"differs in {right}: {child_rel}")
        else:
            problems.append(f"type mismatch in {right}: {child_rel}")

    return problems


def check_targets() -> int:
    expected_roots = {path.name for path in root_files(SHARED)}
    expected_skills = {path.name for path in shared_skill_dirs()}
    problems: list[str] = []

    for target in TARGETS:
        if not target.exists():
            problems.append(f"missing target directory: {target}")
            continue

        target_roots = {path.name for path in root_files(target)}
        target_skills = {
            path.name
            for path in target.iterdir()
            if path.is_dir() and (path / "SKILL.md").is_file()
        }

        for name in sorted(target_roots - expected_roots):
            problems.append(f"extra in {target}: {name}")
        for name in sorted(target_skills - expected_skills):
            problems.append(f"extra in {target}: {name}")

        for name in sorted(expected_roots):
            src = SHARED / name
            dst = target / name
            if not dst.exists():
                problems.append(f"missing in {target}: {name}")
            elif not filecmp.cmp(src, dst, shallow=False):
                problems.append(f"differs in {target}: {name}")

        for name in sorted(expected_skills):
            problems.extend(compare_dirs(SHARED / name, target / name, Path(name)))

    if problems:
        for problem in problems:
            print(problem)
        return 1

    print("agent skills are in sync")
    return 0


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Copy agent-skills/shared into .codex/skills and .claude/skills."
    )
    parser.add_argument(
        "--init-from",
        type=Path,
        help="Seed agent-skills/shared from an existing skills directory.",
    )
    parser.add_argument(
        "--force-init",
        action="store_true",
        help="Allow --init-from to replace an existing shared skills directory.",
    )
    parser.add_argument(
        "--check",
        action="store_true",
        help="Check that generated target skills match agent-skills/shared.",
    )
    return parser.parse_args()


def main() -> int:
    args = parse_args()

    if args.init_from:
        init_shared(args.init_from, args.force_init)

    if args.check:
        return check_targets()

    sync_targets()
    return 0


if __name__ == "__main__":
    sys.exit(main())
