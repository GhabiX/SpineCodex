#!/usr/bin/env python3
"""Create a repository-local Spine Plan Seed task tree."""
from __future__ import annotations

import argparse
from datetime import datetime
from pathlib import Path
import re


def slugify(value: str) -> str:
    slug = re.sub(r"[^A-Za-z0-9_-]+", "_", value.strip()).strip("_")
    if not slug:
        raise ValueError("task name must contain a letter, digit, underscore, or hyphen")
    return slug


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("task_name")
    parser.add_argument("--repo-root", type=Path, default=Path.cwd())
    args = parser.parse_args()
    task_id = f"{slugify(args.task_name)}_{datetime.now().strftime('%Y%m%d_%H%M')}"
    task_root = args.repo_root / ".codex" / "spineplan" / task_id
    if task_root.exists():
        raise SystemExit(f"task already exists: {task_root}")
    (task_root / "nodes").mkdir(parents=True)
    (task_root / "evidence").mkdir()
    (task_root / "artifacts").mkdir()
    (task_root / "tree.yml").write_text(
        f"version: 1\ntask: {task_id}\ngoal: Describe the durable task outcome.\n"
        "active: nodes/root.md\nnodes:\n  nodes/root.md: in_progress\n",
        encoding="utf-8",
    )
    (task_root / "nodes" / "root.md").write_text(
        "# Root\n\n## Goal\n\nDescribe the task outcome.\n\n"
        "## Next\n\nDefine the first concrete action.\n\n## Evidence\n\n- Task tree created.\n",
        encoding="utf-8",
    )
    print(task_root)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
