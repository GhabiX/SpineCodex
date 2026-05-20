use super::ids::NodeId;
use super::plan_bridge::PlanSnapshot;
use super::plan_bridge::PlanTreeCheckpointDraft;
use super::plan_bridge::PlanTreeDraft;
use super::plan_bridge::PlanTreeScopeDraft;
use super::plan_bridge::PlanTreeSnapshot;
use super::runtime::SpineRuntimeError;
use super::state::NodeStatus;
use super::state::SpineState;
use super::store::SpineSidecarStore;
use super::view::display_node_id;
use super::view::parse_display_node_id;
use codex_protocol::plan_tool::PlanItemArg;
use codex_protocol::plan_tool::SpineUpdatePlanArgs;
use codex_protocol::plan_tool::TaskProjectionArg;
use codex_protocol::plan_tool::TaskProjectionDraftNodeArg;
use std::collections::HashSet;

pub(crate) fn record_plan_update(
    state: &SpineState,
    store: &SpineSidecarStore,
    cursor: &NodeId,
    turn_id: String,
    args: SpineUpdatePlanArgs,
) -> Result<PlanSnapshot, SpineRuntimeError> {
    let SpineUpdatePlanArgs {
        mut flat,
        task_projection,
    } = args;
    flat.plan = task_projection.current.checklist.clone();
    let mut plantree = task_projection_to_plantree(state, cursor, task_projection)?;
    let revision = store
        .read_plan_revision(cursor)?
        .unwrap_or(0)
        .checked_add(1)
        .ok_or(SpineRuntimeError::PlanRevisionOverflow)?;
    let event_seq = store.next_tree_event_seq()?;
    let anchor_node_id = plantree.anchor.clone();
    validate_plantree(state, &anchor_node_id, &plantree)?;
    normalize_plantree_node_ids(&mut plantree)?;
    let spine_plantree = Some(PlanTreeSnapshot::from_update(&anchor_node_id, plantree));
    let snapshot =
        PlanSnapshot::from_update(cursor, revision, event_seq, turn_id, flat, spine_plantree);
    store.write_plan_snapshot(cursor, &snapshot)?;
    Ok(snapshot)
}

fn task_projection_to_plantree(
    state: &SpineState,
    cursor: &NodeId,
    task_projection: TaskProjectionArg,
) -> Result<PlanTreeDraft, SpineRuntimeError> {
    let current_node = parse_display_node_id(&task_projection.current.node_id).map_err(|_| {
        SpineRuntimeError::InvalidPlanTree {
            message: format!(
                "invalid task_projection current node id {}",
                task_projection.current.node_id
            ),
        }
    })?;
    if &current_node != cursor {
        return Err(SpineRuntimeError::InvalidPlanTree {
            message: format!(
                "task_projection current node {} must match cursor {}",
                current_node.bracketed(),
                cursor.bracketed()
            ),
        });
    }
    ensure_editable_plantree_node(state, &current_node, "task_projection current")?;

    let draft_scopes = normalize_task_projection_drafts(state, task_projection.draft_nodes)?;
    let mut anchor_candidates = vec![current_node.clone()];
    for draft in &draft_scopes {
        anchor_candidates.push(draft.nearest_real_parent.clone());
    }
    let anchor = lowest_common_ancestor(&anchor_candidates).ok_or_else(|| {
        SpineRuntimeError::InvalidPlanTree {
            message: "task_projection could not resolve an editable anchor".to_string(),
        }
    })?;
    ensure_editable_plantree_node(state, &anchor, "task_projection anchor")?;

    let mut root = PlanTreeScopeDraft {
        node: Some(anchor.to_string()),
        summary: format!("Task projection {}", display_node_id(&anchor)),
        status: None,
        checkpoints: Vec::new(),
        children: Vec::new(),
    };
    populate_task_projection_children(&mut root.children, &anchor, &current_node, &draft_scopes);

    Ok(PlanTreeDraft { anchor, root })
}

fn normalize_task_projection_drafts(
    state: &SpineState,
    drafts: Vec<TaskProjectionDraftNodeArg>,
) -> Result<Vec<ResolvedTaskProjectionDraft>, SpineRuntimeError> {
    let mut known_draft_ids = HashSet::new();
    let mut resolved = Vec::with_capacity(drafts.len());
    for draft in drafts {
        if draft.summary.trim().is_empty() {
            return Err(SpineRuntimeError::InvalidPlanTree {
                message: "task_projection draft summary must not be empty".to_string(),
            });
        }
        let parent = if is_task_projection_draft_id(&draft.parent) {
            if !known_draft_ids.contains(&draft.parent) {
                return Err(SpineRuntimeError::InvalidPlanTree {
                    message: format!(
                        "task_projection draft parent {} must reference an earlier draft_id",
                        draft.parent
                    ),
                });
            }
            TaskProjectionParent::Draft(draft.parent.clone())
        } else {
            let real_parent = parse_display_node_id(&draft.parent).map_err(|_| {
                SpineRuntimeError::InvalidPlanTree {
                    message: format!("invalid task_projection draft parent {}", draft.parent),
                }
            })?;
            ensure_editable_plantree_node(state, &real_parent, "task_projection parent")?;
            TaskProjectionParent::Real(real_parent)
        };
        if let Some(draft_id) = &draft.draft_id {
            if !is_task_projection_draft_id(draft_id) {
                return Err(SpineRuntimeError::InvalidPlanTree {
                    message: format!("task_projection draft_id {draft_id} must start with '~'"),
                });
            }
            if !known_draft_ids.insert(draft_id.clone()) {
                return Err(SpineRuntimeError::InvalidPlanTree {
                    message: format!("task_projection draft_id {draft_id} is duplicated"),
                });
            }
        }
        let nearest_real_parent = match &parent {
            TaskProjectionParent::Real(parent) => parent.clone(),
            TaskProjectionParent::Draft(parent) => resolved
                .iter()
                .find(|resolved: &&ResolvedTaskProjectionDraft| {
                    resolved.draft_id.as_deref() == Some(parent.as_str())
                })
                .map(|resolved| resolved.nearest_real_parent.clone())
                .ok_or_else(|| SpineRuntimeError::InvalidPlanTree {
                    message: format!(
                        "task_projection draft parent {parent} must reference an earlier draft_id"
                    ),
                })?,
        };
        let checkpoints = draft
            .checklist
            .into_iter()
            .map(plan_item_to_plantree_checkpoint)
            .collect();
        resolved.push(ResolvedTaskProjectionDraft {
            draft_id: draft.draft_id,
            parent,
            nearest_real_parent,
            summary: draft.summary,
            checkpoints,
        });
    }
    Ok(resolved)
}

fn populate_task_projection_children(
    children: &mut Vec<PlanTreeScopeDraft>,
    parent: &NodeId,
    current_node: &NodeId,
    drafts: &[ResolvedTaskProjectionDraft],
) {
    if let Some(next_existing) = next_child_on_path(parent, current_node) {
        let mut existing = PlanTreeScopeDraft {
            node: Some(next_existing.to_string()),
            summary: format!("Existing node {}", display_node_id(&next_existing)),
            status: None,
            checkpoints: Vec::new(),
            children: Vec::new(),
        };
        populate_task_projection_children(
            &mut existing.children,
            &next_existing,
            current_node,
            drafts,
        );
        children.push(existing);
    }
    for draft in drafts.iter().filter(|draft| {
        matches!(&draft.parent, TaskProjectionParent::Real(real_parent) if real_parent == parent)
    }) {
        let mut child = PlanTreeScopeDraft {
            node: None,
            summary: draft.summary.clone(),
            status: None,
            checkpoints: draft.checkpoints.clone(),
            children: Vec::new(),
        };
        populate_task_projection_draft_children(&mut child.children, draft, drafts);
        children.push(child);
    }
}

fn populate_task_projection_draft_children(
    children: &mut Vec<PlanTreeScopeDraft>,
    parent: &ResolvedTaskProjectionDraft,
    drafts: &[ResolvedTaskProjectionDraft],
) {
    let Some(parent_draft_id) = parent.draft_id.as_deref() else {
        return;
    };
    for draft in drafts.iter().filter(|draft| {
        matches!(&draft.parent, TaskProjectionParent::Draft(draft_parent) if draft_parent == parent_draft_id)
    }) {
        let mut child = PlanTreeScopeDraft {
            node: None,
            summary: draft.summary.clone(),
            status: None,
            checkpoints: draft.checkpoints.clone(),
            children: Vec::new(),
        };
        populate_task_projection_draft_children(&mut child.children, draft, drafts);
        children.push(child);
    }
}

fn validate_plantree(
    state: &SpineState,
    anchor: &NodeId,
    plantree: &PlanTreeDraft,
) -> Result<(), SpineRuntimeError> {
    let mut existing_scope_nodes = HashSet::new();
    validate_plantree_scope(state, anchor, &plantree.root, &mut existing_scope_nodes)
}

fn validate_plantree_scope(
    state: &SpineState,
    anchor: &NodeId,
    scope: &PlanTreeScopeDraft,
    existing_scope_nodes: &mut HashSet<NodeId>,
) -> Result<(), SpineRuntimeError> {
    if scope.summary.trim().is_empty() {
        return Err(SpineRuntimeError::InvalidPlanTree {
            message: "plan tree scope summary must not be empty".to_string(),
        });
    }
    for checkpoint in &scope.checkpoints {
        if checkpoint.task.trim().is_empty() {
            return Err(SpineRuntimeError::InvalidPlanTree {
                message: "plan tree checkpoint task must not be empty".to_string(),
            });
        }
    }
    if let Some(existing_node_id) = &scope.node {
        let existing_node_id = parse_display_node_id(existing_node_id).map_err(|_| {
            SpineRuntimeError::InvalidPlanTree {
                message: format!("invalid plantree scope node id {existing_node_id}"),
            }
        })?;
        ensure_editable_plantree_node(state, &existing_node_id, "scope")?;
        if !existing_scope_nodes.insert(existing_node_id.clone()) {
            return Err(SpineRuntimeError::InvalidPlanTree {
                message: format!(
                    "plantree scope {} is duplicated",
                    existing_node_id.bracketed()
                ),
            });
        }
        if !is_node_within_anchor(&existing_node_id, anchor) {
            return Err(SpineRuntimeError::InvalidPlanTree {
                message: format!(
                    "plantree scope {} is outside anchor {}",
                    existing_node_id.bracketed(),
                    anchor.bracketed()
                ),
            });
        }
    }
    for child in &scope.children {
        validate_plantree_scope(state, anchor, child, existing_scope_nodes)?;
    }
    Ok(())
}

fn ensure_editable_plantree_node(
    state: &SpineState,
    node_id: &NodeId,
    role: &str,
) -> Result<(), SpineRuntimeError> {
    let node = state
        .node(node_id)
        .ok_or_else(|| SpineRuntimeError::UnknownNode(node_id.clone()))?;
    match node.status {
        NodeStatus::Live | NodeStatus::Opened => Ok(()),
        NodeStatus::Finished | NodeStatus::Closed => Err(SpineRuntimeError::InvalidPlanTree {
            message: format!(
                "plantree {role} {} is read-only because it is {}",
                node_id.bracketed(),
                match node.status {
                    NodeStatus::Finished => "finished",
                    NodeStatus::Closed => "closed",
                    _ => unreachable!("handled editable states above"),
                }
            ),
        }),
    }
}

#[derive(Clone, Debug)]
struct ResolvedTaskProjectionDraft {
    draft_id: Option<String>,
    parent: TaskProjectionParent,
    nearest_real_parent: NodeId,
    summary: String,
    checkpoints: Vec<PlanTreeCheckpointDraft>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum TaskProjectionParent {
    Real(NodeId),
    Draft(String),
}

fn plan_item_to_plantree_checkpoint(item: PlanItemArg) -> PlanTreeCheckpointDraft {
    PlanTreeCheckpointDraft {
        task: item.step,
        status: item.status,
    }
}

fn is_task_projection_draft_id(value: &str) -> bool {
    value.starts_with('~')
}

fn lowest_common_ancestor(nodes: &[NodeId]) -> Option<NodeId> {
    let first = nodes.first()?;
    let mut prefix = first.segments().to_vec();
    for node in &nodes[1..] {
        let common_len = prefix
            .iter()
            .zip(node.segments())
            .take_while(|(left, right)| left == right)
            .count();
        prefix.truncate(common_len);
    }
    if prefix.is_empty() {
        None
    } else {
        Some(NodeId::from_segments(prefix))
    }
}

fn next_child_on_path(parent: &NodeId, descendant: &NodeId) -> Option<NodeId> {
    let parent_len = parent.segments().len();
    let descendant_segments = descendant.segments();
    if descendant_segments.len() <= parent_len {
        return None;
    }
    if !descendant_segments.starts_with(parent.segments()) {
        return None;
    }
    Some(NodeId::from_segments(
        descendant_segments[..=parent_len].to_vec(),
    ))
}

fn normalize_plantree_node_ids(plantree: &mut PlanTreeDraft) -> Result<(), SpineRuntimeError> {
    normalize_plantree_scope_node_ids(&mut plantree.root)
}

fn normalize_plantree_scope_node_ids(
    scope: &mut PlanTreeScopeDraft,
) -> Result<(), SpineRuntimeError> {
    if let Some(node) = &mut scope.node {
        *node = parse_display_node_id(node)
            .map_err(|_| SpineRuntimeError::InvalidPlanTree {
                message: format!("invalid plantree scope node id {node}"),
            })?
            .to_string();
    }
    for child in &mut scope.children {
        normalize_plantree_scope_node_ids(child)?;
    }
    Ok(())
}

fn is_node_within_anchor(node_id: &NodeId, anchor: &NodeId) -> bool {
    node_id.segments().starts_with(anchor.segments())
}
