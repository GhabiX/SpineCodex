use super::ControlSymbol;
use super::ParseStack;
use super::PreparedTaskTreeReduction;
use super::Symbol;
use crate::spine::SpineError;
use crate::spine::archive::SpineArchive;
use crate::spine::archive::archive_task_tree;
use crate::spine::model::MemoryRef;
use crate::spine::model::SpineTreeNode;
use crate::spine::model::TreeMeta;

pub(super) fn reduce_task_tree(
    parse_stack: &mut ParseStack,
    archive: &SpineArchive,
) -> Result<bool, SpineError> {
    match parse_stack.symbols.get(..) {
        Some(
            [
                ..,
                Symbol::Control(ControlSymbol::Open(_)),
                Symbol::Control(ControlSymbol::Close(_)),
            ],
        ) => {
            return Err(SpineError::InvalidEvent(
                "spine.close requires non-empty live suffix".to_string(),
            ));
        }
        Some(
            [
                ..,
                Symbol::Control(ControlSymbol::Open(_)),
                Symbol::SpineTreeNodes(_),
                Symbol::Control(ControlSymbol::Close(_)),
            ],
        ) => {}
        _ => return Ok(false),
    }
    let len = parse_stack.symbols.len();
    let (
        Symbol::Control(ControlSymbol::Open(meta)),
        Symbol::SpineTreeNodes(children),
        Symbol::Control(ControlSymbol::Close(memory)),
    ) = (
        parse_stack.symbols[len - 3].clone(),
        parse_stack.symbols[len - 2].clone(),
        parse_stack.symbols[len - 1].clone(),
    )
    else {
        unreachable!("close reduction suffix was checked before clone")
    };
    let (memory_path, trajs_path) = archive_task_tree(archive, &meta, &children, &memory)?;
    parse_stack.symbols.truncate(len - 3);
    parse_stack
        .symbols
        .push(Symbol::SpineTreeNode(SpineTreeNode::SpineTree {
            memory,
            meta,
            children,
            memory_path,
            trajs_path,
        }));
    Ok(true)
}

pub(super) fn prepare_current_task_tree_reduction(
    parse_stack: &ParseStack,
    archive: &SpineArchive,
    memory: MemoryRef,
) -> Result<PreparedTaskTreeReduction, SpineError> {
    let Some((meta, children)) = current_task_tree_suffix(parse_stack) else {
        return Err(SpineError::InvalidEvent(
            "spine.close requires a live task tree suffix".to_string(),
        ));
    };
    validate_pending_task_tree_reduction_memory(parse_stack, &memory)?;
    let (memory_path, trajs_path) = archive_task_tree(archive, meta, children, &memory)?;
    Ok(PreparedTaskTreeReduction {
        meta: meta.clone(),
        children: children.to_vec(),
        memory,
        memory_path,
        trajs_path,
    })
}

pub(super) fn shift_pending_close(
    parse_stack: &mut ParseStack,
    memory: MemoryRef,
    archive: &SpineArchive,
) -> Result<(), SpineError> {
    if pending_close_memory(parse_stack)?.is_some() {
        validate_pending_task_tree_reduction_memory(parse_stack, &memory)?;
        return Ok(());
    }
    parse_stack.reduce_fixpoint(archive)?;
    if current_task_tree_suffix(parse_stack).is_none() {
        return Err(SpineError::InvalidEvent(
            "spine.close requires a live task tree suffix".to_string(),
        ));
    }
    parse_stack
        .symbols
        .push(Symbol::Control(ControlSymbol::Close(memory)));
    Ok(())
}

pub(super) fn validate_pending_task_tree_reduction(
    parse_stack: &ParseStack,
    reduction: &PreparedTaskTreeReduction,
) -> Result<(), SpineError> {
    let Some(
        [
            ..,
            Symbol::Control(ControlSymbol::Open(meta)),
            Symbol::SpineTreeNodes(children),
            Symbol::Control(ControlSymbol::Close(memory)),
        ],
    ) = parse_stack.symbols.get(..)
    else {
        return Err(SpineError::InvalidEvent(
            "spine.close reduction requires a pending Close suffix".to_string(),
        ));
    };
    if meta != &reduction.meta || children != &reduction.children || memory != &reduction.memory {
        return Err(SpineError::InvalidEvent(
            "pending spine.close suffix changed before reduction".to_string(),
        ));
    }
    Ok(())
}

pub(super) fn apply_prevalidated_task_tree_reduction(
    parse_stack: &mut ParseStack,
    reduction: PreparedTaskTreeReduction,
) {
    debug_assert!(validate_pending_task_tree_reduction(parse_stack, &reduction).is_ok());
    let len = parse_stack.symbols.len();
    parse_stack.symbols.truncate(len - 3);
    parse_stack
        .symbols
        .push(Symbol::SpineTreeNode(SpineTreeNode::SpineTree {
            memory: reduction.memory,
            meta: reduction.meta,
            children: reduction.children,
            memory_path: reduction.memory_path,
            trajs_path: reduction.trajs_path,
        }));
    parse_stack.reduce_nodes_fixpoint();
}

pub(super) fn task_tree_reduced(
    parse_stack: &ParseStack,
    reduction: PreparedTaskTreeReduction,
) -> Result<ParseStack, SpineError> {
    validate_pending_task_tree_reduction(parse_stack, &reduction)?;
    let mut reduced = parse_stack.clone();
    apply_prevalidated_task_tree_reduction(&mut reduced, reduction);
    Ok(reduced)
}

fn current_task_tree_suffix(parse_stack: &ParseStack) -> Option<(&TreeMeta, &[SpineTreeNode])> {
    match parse_stack.symbols.get(..) {
        Some(
            [
                ..,
                Symbol::Control(ControlSymbol::Open(meta)),
                Symbol::SpineTreeNodes(children),
            ]
            | [
                ..,
                Symbol::Control(ControlSymbol::Open(meta)),
                Symbol::SpineTreeNodes(children),
                Symbol::Control(ControlSymbol::Close(_)),
            ],
        ) if !children.is_empty() => Some((meta, children)),
        _ => None,
    }
}

fn pending_close_memory(parse_stack: &ParseStack) -> Result<Option<&MemoryRef>, SpineError> {
    match parse_stack.symbols.get(..) {
        Some(
            [
                ..,
                Symbol::Control(ControlSymbol::Open(_)),
                Symbol::SpineTreeNodes(_),
                Symbol::Control(ControlSymbol::Close(memory)),
            ],
        ) => Ok(Some(memory)),
        Some(
            [
                ..,
                Symbol::Control(ControlSymbol::Open(_)),
                Symbol::Control(ControlSymbol::Close(_)),
            ],
        ) => Err(SpineError::InvalidEvent(
            "spine.close requires non-empty live suffix".to_string(),
        )),
        _ => Ok(None),
    }
}

fn validate_pending_task_tree_reduction_memory(
    parse_stack: &ParseStack,
    memory: &MemoryRef,
) -> Result<(), SpineError> {
    let Some(existing) = pending_close_memory(parse_stack)? else {
        return Ok(());
    };
    if existing != memory {
        return Err(SpineError::InvalidEvent(format!(
            "pending spine.close memory {} does not match prepared memory {}",
            existing.compact_id, memory.compact_id
        )));
    }
    Ok(())
}
