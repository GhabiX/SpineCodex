use crate::spine::SpineError;
use crate::spine::checkpoint::CheckpointMemoryRef;
use crate::spine::checkpoint::SpineCheckpoint;
use crate::spine::compact_checkpoint::SpineCompactCheckpoint;
use crate::spine::model::ControlSymbol;
use crate::spine::model::MemoryRef;
use crate::spine::model::RootEpoch;
use crate::spine::model::SegRef;
use crate::spine::model::SpineCommitMarker;
use crate::spine::model::SpineCommitMemoryRef;
use crate::spine::model::SpineTreeNode;
use crate::spine::model::Symbol;
use crate::spine::model::TreeMeta;
use std::collections::BTreeMap;
use std::path::Path;
use std::path::PathBuf;

pub(super) fn clone_compact_checkpoint_for_target(
    checkpoint: SpineCompactCheckpoint,
    target_rollout_path: &Path,
    cloned_memory_paths: &BTreeMap<String, String>,
) -> Result<SpineCompactCheckpoint, SpineError> {
    let mut memory_refs = Vec::with_capacity(checkpoint.memory_refs.len());
    for memory in checkpoint.memory_refs {
        let body_path = cloned_memory_paths
            .get(&memory.compact_id)
            .ok_or_else(|| {
                SpineError::InvalidStore(format!(
                    "compact checkpoint references uncloned memory {}",
                    memory.compact_id
                ))
            })?
            .clone();
        memory_refs.push(CheckpointMemoryRef {
            body_path,
            ..memory
        });
    }
    Ok(SpineCompactCheckpoint {
        rollout_path: target_rollout_path.display().to_string(),
        memory_refs,
        ..checkpoint
    })
}

pub(super) fn clone_checkpoint_for_target(
    mut checkpoint: SpineCheckpoint,
    target_rollout_path: &Path,
    target_root: &Path,
    cloned_memory_paths: &BTreeMap<String, String>,
) -> Result<SpineCheckpoint, SpineError> {
    checkpoint.rollout_path = target_rollout_path.display().to_string();
    for memory in &mut checkpoint.memory_refs {
        memory.body_path =
            cloned_memory_body_path(target_root, cloned_memory_paths, &memory.compact_id)?
                .display()
                .to_string();
    }
    for tree_meta in &mut checkpoint.tree_meta {
        tree_meta.node_dir = checkpoint_node_dir(target_root, &tree_meta.id)
            .display()
            .to_string();
    }
    for trajs_ref in &mut checkpoint.trajs_refs {
        trajs_ref.trajs_path = checkpoint_node_archive_path(&trajs_ref.node_id, "Trajs.md")
            .display()
            .to_string();
    }
    rewrite_checkpoint_symbols_for_target(
        &mut checkpoint.parse_stack.symbols,
        target_root,
        cloned_memory_paths,
    )?;
    checkpoint.parse_stack_symbols = checkpoint
        .parse_stack
        .symbols
        .iter()
        .map(|symbol| format!("{symbol:?}"))
        .collect();
    Ok(checkpoint)
}

fn rewrite_checkpoint_symbols_for_target(
    symbols: &mut [Symbol],
    target_root: &Path,
    cloned_memory_paths: &BTreeMap<String, String>,
) -> Result<(), SpineError> {
    for symbol in symbols {
        match symbol {
            Symbol::Control(control) => {
                rewrite_checkpoint_control_for_target(control, target_root, cloned_memory_paths)?
            }
            Symbol::SpineTreeNode(node) => {
                rewrite_checkpoint_node_for_target(node, target_root, cloned_memory_paths)?
            }
            Symbol::SpineTreeNodes(nodes) => {
                for node in nodes {
                    rewrite_checkpoint_node_for_target(node, target_root, cloned_memory_paths)?;
                }
            }
            Symbol::RootEpoches(root_epochs) => {
                for RootEpoch { memory } in root_epochs {
                    rewrite_checkpoint_memory_ref_for_target(
                        memory,
                        target_root,
                        cloned_memory_paths,
                    )?;
                }
            }
        }
    }
    Ok(())
}

fn rewrite_checkpoint_control_for_target(
    control: &mut ControlSymbol,
    target_root: &Path,
    cloned_memory_paths: &BTreeMap<String, String>,
) -> Result<(), SpineError> {
    match control {
        ControlSymbol::Init(meta) | ControlSymbol::Open(meta) => {
            rewrite_checkpoint_tree_meta_for_target(meta, target_root);
            Ok(())
        }
        ControlSymbol::End => Ok(()),
        ControlSymbol::Close(memory) | ControlSymbol::Compact(memory, _, _, _) => {
            rewrite_checkpoint_memory_ref_for_target(memory, target_root, cloned_memory_paths)
        }
    }
}

fn rewrite_checkpoint_node_for_target(
    node: &mut SpineTreeNode,
    target_root: &Path,
    cloned_memory_paths: &BTreeMap<String, String>,
) -> Result<(), SpineError> {
    match node {
        SpineTreeNode::MsgAsLeafNode { msg, .. } => {
            rewrite_checkpoint_seg_ref_for_target(msg, target_root, cloned_memory_paths)
        }
        SpineTreeNode::ToolCallAsLeafNode { segments } => {
            for segment in segments {
                rewrite_checkpoint_seg_ref_for_target(
                    &mut segment.seg,
                    target_root,
                    cloned_memory_paths,
                )?;
            }
            Ok(())
        }
        SpineTreeNode::SpineTree {
            memory,
            meta,
            children,
            memory_path,
            trajs_path,
        } => {
            rewrite_checkpoint_memory_ref_for_target(memory, target_root, cloned_memory_paths)?;
            rewrite_checkpoint_tree_meta_for_target(meta, target_root);
            for child in children {
                rewrite_checkpoint_node_for_target(child, target_root, cloned_memory_paths)?;
            }
            let node_id = meta.id.as_path();
            *memory_path = checkpoint_node_archive_path(&node_id, "Memory.md");
            *trajs_path = checkpoint_node_archive_path(&node_id, "Trajs.md");
            Ok(())
        }
    }
}

fn rewrite_checkpoint_tree_meta_for_target(meta: &mut TreeMeta, target_root: &Path) {
    meta.node_dir = checkpoint_node_dir(target_root, &meta.id.as_path());
}

fn rewrite_checkpoint_memory_ref_for_target(
    memory: &mut MemoryRef,
    target_root: &Path,
    cloned_memory_paths: &BTreeMap<String, String>,
) -> Result<(), SpineError> {
    memory.body_path =
        cloned_memory_body_path(target_root, cloned_memory_paths, &memory.compact_id)?;
    Ok(())
}

fn rewrite_checkpoint_seg_ref_for_target(
    seg: &mut SegRef,
    target_root: &Path,
    cloned_memory_paths: &BTreeMap<String, String>,
) -> Result<(), SpineError> {
    match seg {
        SegRef::ResponseItem { .. } => Ok(()),
        SegRef::Memory {
            memory_id,
            body_path,
        } => {
            *body_path = cloned_memory_body_path(target_root, cloned_memory_paths, memory_id)?;
            Ok(())
        }
    }
}

fn cloned_memory_body_path(
    target_root: &Path,
    cloned_memory_paths: &BTreeMap<String, String>,
    compact_id: &str,
) -> Result<PathBuf, SpineError> {
    cloned_memory_paths
        .get(compact_id)
        .map(|path| target_root.join(path))
        .ok_or_else(|| {
            SpineError::InvalidStore(format!(
                "checkpoint references uncloned memory {compact_id}"
            ))
        })
}

fn checkpoint_node_dir(target_root: &Path, node_id: &str) -> PathBuf {
    target_root.join("nodes").join(node_id.replace('.', "/"))
}

fn checkpoint_node_archive_path(node_id: &str, file_name: &str) -> PathBuf {
    PathBuf::from("nodes")
        .join(node_id.replace('.', "/"))
        .join(file_name)
}

pub(super) fn clone_commit_marker_for_target(
    marker: SpineCommitMarker,
    cloned_memory_paths: &BTreeMap<String, String>,
) -> Result<SpineCommitMarker, SpineError> {
    let mut memory_refs = Vec::with_capacity(marker.memory_refs.len());
    for memory in marker.memory_refs {
        let body_path = cloned_memory_paths
            .get(&memory.compact_id)
            .ok_or_else(|| {
                SpineError::InvalidStore(format!(
                    "Spine commit marker references uncloned memory {}",
                    memory.compact_id
                ))
            })?
            .clone();
        memory_refs.push(SpineCommitMemoryRef {
            body_path,
            ..memory
        });
    }
    Ok(SpineCommitMarker {
        memory_refs,
        ..marker
    })
}
