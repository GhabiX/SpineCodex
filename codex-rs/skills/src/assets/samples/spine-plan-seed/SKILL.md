---
name: spine-plan-seed
description: >
  Use for long-running, complex, or exploratory repository work that benefits
  from a lightweight durable task tree, typically under ./.codex/spineplan/, including
  plans that must evolve as unknowns are discovered.
---

# Spine Plan Seed

Treat the plan as the current best semantic model of the work, not an upfront
contract. Refine it as evidence exposes previously unknown work or invalidates
earlier assumptions.

## Task tree

Create the task tree in a suitable repository-local location, defaulting to:

```text
./.codex/spineplan/{task_name}_{YYYYMMDD_HHmm}/
```

The root contains `tree.yml`, `nodes/`, `evidence/`, and `artifacts/`. Each node is a `node.md`; child directories represent recursive decomposition. `tree.yml` is the authority for the active node and node statuses. The recursive `node.md` files hold node-local details; do not duplicate those details in the index.

Change the topology whenever the current tree no longer represents the work
well: add, split, merge, prune, rename, or reparent nodes. Preserve useful
evidence when replacing a node, then reconcile `tree.yml`, node paths, and
`parent` links. Prefer the smallest tree whose semantics, boundaries, and depth
remain clear.

## Node state

Keep each node focused on one coherent, independently verifiable outcome.
Create children when a goal contains distinct obligations, decisions, risks,
or evidence lanes that benefit from separate reasoning. Merge or prune nodes
when those boundaries no longer help. Do not mirror a chronological checklist,
tool calls, or Spine transitions unless each resulting node owns a meaningful
outcome.

`tree.yml` records each node path with status `pending`, `in_progress`,
`blocked`, or `done`. A `node.md` records `parent`, `Goal`, `Next`, and
`Evidence`.

Mark a node `done` only after its goal is satisfied and relevant evidence is recorded. Mark it `blocked` with the reason in the node when work cannot continue. Keep at most one `in_progress` node, and make `active` point to it. The active node should normally be the most specific node currently being advanced. Creating a child registers it as `pending`; when work moves into that child, move `in_progress` and `active` to it instead of leaving its parent active.

## Maintenance

The agent explicitly maintains `tree.yml` and the node files as ordinary repository files. Update the plan when scope, structure, status, or evidence materially changes, and before pausing, handing off, compacting, or completing work. Do not churn the plan for routine actions. The bundled scripts create nodes and check mechanical consistency; they do not infer, repair, or approve task meaning, boundaries, topology, or completion.

Spine and the durable plan have different jobs. Spine scopes live model context;
the plan records durable task semantics, status, and evidence. Do not mirror
their topology. Several Spine nodes may advance one plan node, one Spine node
may advance successive plan nodes, and temporary Spine nodes may have no plan
counterpart. Keep durable state in the plan files and only the active plan path
plus minimum continuation context in Spine memory.

To resume, read `tree.yml`, the active node, and only the ancestor context needed
to recover its boundary. Refine the tree before continuing if current evidence
has made it stale. Run `check_plan.py` after structural edits and before a
handoff or completion; a passing check establishes only mechanical consistency,
not plan quality.

## Scripts

Run the bundled scripts from the installed skill directory supplied by Codex:

```bash
python scripts/init_plan.py <task-name>
python scripts/add_node.py <task-root> <parent-node> <node-name>
python scripts/check_plan.py <task-root>
```

The scripts intentionally cover initialization, child creation, and mechanical
checking only. Perform semantic restructuring directly in the plan files.

See `references/schema.md` for the minimal file format.
