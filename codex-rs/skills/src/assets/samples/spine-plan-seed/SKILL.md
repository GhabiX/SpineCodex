---
name: spine-plan-seed
description: >
  Use for long-running, complex, or exploratory repository work that benefits
  from a lightweight durable task tree under ./.codex/spineplan/.
---

# Spine Plan Seed

Use this skill for long-running, complex, or exploratory work where the problem, solution, or execution path is not fully known in advance. Do not use it for trivial questions or small one-step edits.

## Task tree

Create the task tree under the current repository:

```text
./.codex/spineplan/{task_name}_{YYYYMMDD_HHmm}/
```

The root contains `tree.yml`, `nodes/`, `evidence/`, and `artifacts/`. Each node is a `node.md`; child directories represent recursive decomposition. `tree.yml` is the authority for the active node and node statuses. The recursive `node.md` files hold node-local details; do not duplicate those details in the index.

## Node state

Keep each node focused on one verifiable goal. `tree.yml` records each node path with status `pending`, `in_progress`, `blocked`, or `done`. A `node.md` records `parent`, `Goal`, `Next`, and `Evidence`.

Mark a node `done` only after its goal is satisfied and relevant evidence is recorded. Mark it `blocked` with the reason in the node when work cannot continue. Keep at most one `in_progress` node, and make `active` point to it. Creating a child registers it as `pending` and does not change the active node.

## Maintenance

The agent explicitly maintains `tree.yml` and the node files as ordinary repository files. Update them before pausing, handing off, compacting, or completing work. Use the bundled scripts to create and mechanically validate trees; the scripts do not infer, repair, or decide task meaning or completion.

Spine tree is naturally both a living-context management tree and a task-context tree. Use it together with this durable plan tree rather than treating them as separate competing plans: a Spine node may advance one plan node, several Spine nodes may advance the same plan node, and temporary Spine nodes may have no plan-node counterpart. Keep durable status and evidence in the plan files; preserve the current plan-node path and only the minimum continuation context in Spine memory. Before a Spine transition that changes engineering scope, or before compaction, handoff, or pause, update the corresponding plan node.

To resume, read `tree.yml`, then read the node named by `active`, and run `check_plan.py` before continuing. Update `tree.yml` explicitly when the engineering scope or a node status changes.

## Scripts

Run the bundled scripts from the installed skill directory supplied by Codex:

```bash
python scripts/init_plan.py <task-name>
python scripts/add_node.py <task-root> <parent-node> <node-name>
python scripts/check_plan.py <task-root>
```

See `references/schema.md` for the minimal file format.
