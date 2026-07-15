#!/usr/bin/env python3
"""Add a child node."""
from __future__ import annotations

import argparse
import os
from pathlib import Path
import re

NODE_ENTRY_RE = re.compile(r"^  ([^:]+):\s*(\S+)\s*$")


def slugify(value: str) -> str:
    slug = re.sub(r"[^A-Za-z0-9_-]+", "_", value.strip()).strip("_")
    if not slug:
        raise ValueError("node name must contain a letter, digit, underscore, or hyphen")
    return slug


def read_registered_nodes(tree_text: str) -> set[str]:
    nodes: set[str] = set()
    in_nodes = False
    saw_nodes = False
    for line in tree_text.splitlines():
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
            raise SystemExit(f"invalid nodes entry in tree.yml: {line}")
        nodes.add(match.group(1).strip())
    if not saw_nodes:
        raise SystemExit("tree.yml has no nodes map")
    return nodes


def add_registered_node(tree_text: str, node_path: str) -> str:
    lines = tree_text.splitlines()
    try:
        nodes_index = lines.index("nodes:")
    except ValueError as error:
        raise SystemExit("tree.yml has no nodes map") from error
    insert_at = len(lines)
    for index in range(nodes_index + 1, len(lines)):
        if lines[index] and not lines[index].startswith(" "):
            insert_at = index
            break
    lines.insert(insert_at, f"  {node_path}: pending")
    return "\n".join(lines) + "\n"


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("task_root", type=Path)
    parser.add_argument("parent_node")
    parser.add_argument("node_name")
    args = parser.parse_args()
    task_root = args.task_root.resolve()
    tree = task_root / "tree.yml"
    if not tree.is_file():
        raise SystemExit(f"tree.yml does not exist: {tree}")
    tree_text = tree.read_text(encoding="utf-8")
    registered_nodes = read_registered_nodes(tree_text)
    parent = (task_root / args.parent_node).resolve()
    if parent.suffix != ".md" or not parent.is_file():
        raise SystemExit(f"parent node does not exist: {parent}")
    parent_relative_to_root = parent.relative_to(task_root).as_posix()
    if parent_relative_to_root not in registered_nodes:
        raise SystemExit(f"parent node is not registered: {parent_relative_to_root}")
    parent_dir = parent.parent / parent.stem if parent.name != "node.md" else parent.parent
    node_dir = parent_dir / slugify(args.node_name)
    node_path = node_dir / "node.md"
    node_relative_to_root = node_path.relative_to(task_root).as_posix()
    if node_relative_to_root in registered_nodes:
        raise SystemExit(f"node is already registered: {node_relative_to_root}")
    if node_path.exists():
        raise SystemExit(f"node already exists: {node_path}")
    updated_tree_text = add_registered_node(tree_text, node_relative_to_root)
    node_dir.mkdir(parents=True)
    parent_relative = Path(os.path.relpath(parent, node_dir))
    node_path.write_text(
        f"# {args.node_name}\n\nparent: {parent_relative.as_posix()}\n\n"
        "## Goal\n\nDescribe one concrete sub-goal.\n\n## Next\n\nDefine the next action.\n\n"
        "## Evidence\n\n- None yet.\n",
        encoding="utf-8",
    )
    tree.write_text(updated_tree_text, encoding="utf-8")
    print(node_path.relative_to(task_root))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
