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

Spine tree is naturally both a living-context management tree and a task-context tree. The active plan node is the durable engineering scope being advanced by the current Spine node. The mapping is many-to-one rather than mechanical: multiple Spine nodes may work on one plan node, while temporary Spine nodes may have no plan-node counterpart. Keep only the active plan-node path and the minimum continuation context in Spine memory; keep durable status and evidence in `node.md`.

The plan tree supplies durable repository state, while Spine supplies the live context needed to reason and act now. They should cooperate through the active plan-node path, not duplicate every node or require runtime ownership of the plan files.

The task tree has at most one `in_progress` node. `tree.yml.active` must point to that node. The agent updates this state explicitly; scripts only create entries and check consistency.
