#!/usr/bin/env python3
"""Check mechanical task-tree invariants."""
from __future__ import annotations

import argparse
from pathlib import Path
import re
import sys


PARENT_RE = re.compile(r"^parent:\s*(\S+)\s*$", re.MULTILINE)
NODE_ENTRY_RE = re.compile(r"^  ([^:]+):\s*(\S+)\s*$")
VALID_STATUSES = {"pending", "in_progress", "blocked", "done"}


def parse_tree(tree: Path, errors: list[str]) -> tuple[str | None, dict[str, str]]:
    active: str | None = None
    nodes: dict[str, str] = {}
    in_nodes = False
    saw_active = False
    saw_nodes = False
    for line_number, line in enumerate(tree.read_text(encoding="utf-8").splitlines(), 1):
        if line.startswith("active:"):
            saw_active = True
            value = line.split(":", 1)[1].strip()
            active = None if value in {"", "null", "~"} else value
            continue
        if line == "nodes:":
            saw_nodes = True
            in_nodes = True
            continue
        if in_nodes and line and not line.startswith(" "):
            in_nodes = False
        if not in_nodes or not line.strip():
            continue
        match = NODE_ENTRY_RE.fullmatch(line)
        if not match:
            errors.append(f"tree.yml:{line_number} has an invalid nodes entry")
            continue
        node_path = match.group(1).strip()
        status = match.group(2)
        if node_path in nodes:
            errors.append(f"tree.yml has duplicate node path: {node_path}")
        nodes[node_path] = status
    if not saw_active:
        errors.append("tree.yml has no active path")
    if not saw_nodes:
        errors.append("tree.yml has no nodes map")
    return active, nodes


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("task_root", type=Path)
    args = parser.parse_args()
    root = args.task_root.resolve()
    errors: list[str] = []
    tree = root / "tree.yml"
    if not tree.is_file():
        errors.append("missing tree.yml")
    active = None
    registered_nodes: dict[str, str] = {}
    if tree.is_file():
        active, registered_nodes = parse_tree(tree, errors)
    nodes = sorted(
        set((root / "nodes").rglob("node.md"))
        | set((root / "nodes").glob("*.md"))
    )
    if not nodes:
        errors.append("no node.md files found")
    node_set = set(nodes)
    for node in nodes:
        contents = node.read_text(encoding="utf-8")
        for section in ("## Goal", "## Next", "## Evidence"):
            if section not in contents:
                errors.append(f"{node.relative_to(root)} is missing {section}")
        if node != root / "nodes" / "root.md":
            parent_match = PARENT_RE.search(contents)
            if not parent_match:
                errors.append(f"{node.relative_to(root)} has no parent")
            else:
                parent = (node.parent / parent_match.group(1)).resolve()
                if parent not in node_set:
                    errors.append(
                        f"{node.relative_to(root)} has invalid parent: {parent_match.group(1)}"
                    )
    discovered_paths = {node.relative_to(root).as_posix() for node in nodes}
    registered_paths = set(registered_nodes)
    for node_path in sorted(registered_paths - discovered_paths):
        errors.append(f"registered node does not exist: {node_path}")
    for node_path in sorted(discovered_paths - registered_paths):
        errors.append(f"node is not registered: {node_path}")
    for node_path, status in registered_nodes.items():
        if status not in VALID_STATUSES:
            errors.append(f"{node_path} has invalid status: {status}")
    in_progress = [
        node_path
        for node_path, status in registered_nodes.items()
        if status == "in_progress"
    ]
    if len(in_progress) > 1:
        errors.append("more than one in_progress node")
    if active is None and in_progress:
        errors.append("tree.yml active is null but a node is in_progress")
    elif active is not None:
        if active not in registered_nodes:
            errors.append(f"active path is not registered: {active}")
        elif registered_nodes[active] != "in_progress":
            errors.append(f"tree.yml active node is not in_progress: {active}")
        if in_progress and in_progress != [active]:
            errors.append(f"in_progress node does not match active: {in_progress[0]}")
    if errors:
        for error in errors:
            print(f"error: {error}", file=sys.stderr)
        return 1
    print(f"ok: {root}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
