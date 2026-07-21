# Spine Plan Seed Schema

## Layout

```text
./.codex/spineplan/{task_name}_{YYYYMMDD_HHmm}/
├── tree.yml
├── nodes/
├── evidence/
└── artifacts/
```

## tree.yml

```yaml
version: 1
task: example_20260713_1200
goal: Describe the durable task outcome.
active: nodes/root.md
nodes:
  nodes/root.md: in_progress
```

`active` is the repository-relative path of the unique `in_progress` node, or `null` when no node is in progress. `nodes` maps every node path to `pending`, `in_progress`, `blocked`, or `done`. `tree.yml` is the status authority; it does not duplicate node goals, evidence, or next actions.

## node.md

```markdown
# Node title

parent: ../root.md

## Goal

One concrete, verifiable sub-goal.

## Next

The next concrete action.

## Evidence

- Evidence, artifacts, or validation results.
```

Recursive nodes use a directory containing `node.md`; their child nodes are directories below it. A root-level node may omit `parent`.

## Spine coordination

Spine scopes live model context; the plan persists task semantics, status, and
evidence. Their topologies need not match. Keep only the active plan-node path
and minimum continuation context in Spine memory; keep durable state in the
plan files.

The task tree has at most one `in_progress` node. `tree.yml.active` must point to that node, normally the most specific node currently being advanced. A parent with unfinished children may remain `pending`; it should not remain active while a child is in progress.

## Structural edits

After adding, splitting, merging, pruning, renaming, or reparenting nodes,
update the `nodes` map, paths, parent links, status, and `active` together.
`check_plan.py` checks only their mechanical consistency.
