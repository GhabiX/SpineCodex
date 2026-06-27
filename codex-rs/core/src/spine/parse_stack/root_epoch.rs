use super::ControlSymbol;
use super::ParseStack;
use super::PreparedRootEpochReduction;
use super::Symbol;
use crate::spine::SpineError;
use crate::spine::archive::SpineArchive;
use crate::spine::archive::next_root_open_symbol;
use crate::spine::model::MemoryRef;
use crate::spine::model::RootEpoch;

pub(super) fn reduce_root_epoch(
    parse_stack: &mut ParseStack,
    archive: &SpineArchive,
) -> Result<bool, SpineError> {
    let Some(compact_idx) = parse_stack
        .symbols
        .iter()
        .rposition(|symbol| matches!(symbol, Symbol::Control(ControlSymbol::Compact(..))))
    else {
        return Ok(false);
    };
    let Symbol::Control(ControlSymbol::Compact(
        memory,
        next_open_index,
        next_open_input_tokens,
        next_open_context_tokens,
    )) = parse_stack.symbols[compact_idx].clone()
    else {
        unreachable!("compact symbol was checked before clone")
    };
    let next_open = next_root_open_symbol(
        archive,
        &memory,
        next_open_index,
        next_open_input_tokens,
        next_open_context_tokens,
    )?;
    let Some(boundary_idx) = parse_stack.symbols[..compact_idx]
        .iter()
        .rposition(|symbol| {
            matches!(
                symbol,
                Symbol::Control(ControlSymbol::Init(_)) | Symbol::RootEpoches(_)
            )
        })
    else {
        return Ok(false);
    };

    let root_epoch = RootEpoch { memory };
    let boundary = parse_stack.symbols[boundary_idx].clone();
    apply_root_epoch_boundary(parse_stack, boundary_idx, boundary, root_epoch);
    parse_stack.symbols.push(next_open);
    Ok(true)
}

pub(super) fn prepare_root_epoch_reduction(
    parse_stack: &ParseStack,
    archive: &SpineArchive,
    memory: MemoryRef,
    next_open_index: usize,
    next_open_input_tokens: Option<i64>,
    next_open_context_tokens: Option<i64>,
) -> Result<PreparedRootEpochReduction, SpineError> {
    let next_open = next_root_open_symbol(
        archive,
        &memory,
        next_open_index,
        next_open_input_tokens,
        next_open_context_tokens,
    )?;
    let Some(boundary_idx) = parse_stack.symbols.iter().rposition(|symbol| {
        matches!(
            symbol,
            Symbol::Control(ControlSymbol::Init(_)) | Symbol::RootEpoches(_)
        )
    }) else {
        return Err(SpineError::InvalidEvent(
            "root compact has no root epoch boundary".to_string(),
        ));
    };
    let compact_idx = if pending_compact_next_open_index(
        parse_stack,
        &memory,
        next_open_input_tokens,
        next_open_context_tokens,
    )?
    .is_some()
    {
        parse_stack.symbols.len() - 1
    } else {
        parse_stack.symbols.len()
    };
    Ok(PreparedRootEpochReduction {
        compact_idx,
        boundary_idx,
        boundary: parse_stack.symbols[boundary_idx].clone(),
        root_epoch: RootEpoch { memory },
        next_open,
    })
}

pub(super) fn shift_pending_compact(
    parse_stack: &mut ParseStack,
    memory: MemoryRef,
    next_open_index: usize,
    next_open_input_tokens: Option<i64>,
    next_open_context_tokens: Option<i64>,
    archive: &SpineArchive,
) -> Result<(), SpineError> {
    if let Some(existing) = pending_compact_memory(parse_stack) {
        validate_pending_compact_memory(existing, &memory)?;
        return Ok(());
    }
    parse_stack.reduce_fixpoint(archive)?;
    parse_stack
        .symbols
        .push(Symbol::Control(ControlSymbol::Compact(
            memory,
            next_open_index,
            next_open_input_tokens,
            next_open_context_tokens,
        )));
    Ok(())
}

pub(super) fn validate_pending_root_epoch_reduction(
    parse_stack: &ParseStack,
    reduction: &PreparedRootEpochReduction,
) -> Result<(), SpineError> {
    let Some(Symbol::Control(ControlSymbol::Compact(
        memory,
        next_open_index,
        next_open_input_tokens,
        next_open_context_tokens,
    ))) = parse_stack.symbols.get(reduction.compact_idx)
    else {
        return Err(SpineError::InvalidEvent(
            "root compact reduction requires a pending Compact token".to_string(),
        ));
    };
    let Symbol::Control(ControlSymbol::Open(next_open)) = &reduction.next_open else {
        return Err(SpineError::Invariant(
            "root compact prepared next open is not an Open symbol".to_string(),
        ));
    };
    if &reduction.root_epoch.memory != memory
        || next_open.index != *next_open_index
        || next_open.open_input_tokens != *next_open_input_tokens
        || next_open.open_context_tokens != *next_open_context_tokens
    {
        return Err(SpineError::InvalidEvent(
            "pending root compact token changed before reduction".to_string(),
        ));
    }
    Ok(())
}

pub(super) fn apply_prevalidated_root_epoch_reduction(
    parse_stack: &mut ParseStack,
    reduction: PreparedRootEpochReduction,
) {
    debug_assert!(validate_pending_root_epoch_reduction(parse_stack, &reduction).is_ok());
    apply_root_epoch_boundary(
        parse_stack,
        reduction.boundary_idx,
        reduction.boundary,
        reduction.root_epoch,
    );
    parse_stack.symbols.push(reduction.next_open);
}

pub(super) fn root_epoch_reduced(
    parse_stack: &ParseStack,
    reduction: PreparedRootEpochReduction,
) -> Result<ParseStack, SpineError> {
    validate_pending_root_epoch_reduction(parse_stack, &reduction)?;
    let mut reduced = parse_stack.clone();
    apply_prevalidated_root_epoch_reduction(&mut reduced, reduction);
    Ok(reduced)
}

pub(super) fn pending_compact_next_open_index(
    parse_stack: &ParseStack,
    memory: &MemoryRef,
    next_open_input_tokens: Option<i64>,
    next_open_context_tokens: Option<i64>,
) -> Result<Option<usize>, SpineError> {
    let Some(Symbol::Control(ControlSymbol::Compact(
        existing,
        next_open_index,
        existing_input_tokens,
        existing_context_tokens,
    ))) = parse_stack.symbols.last()
    else {
        return Ok(None);
    };
    if existing != memory
        || *existing_input_tokens != next_open_input_tokens
        || *existing_context_tokens != next_open_context_tokens
    {
        return Err(SpineError::InvalidEvent(format!(
            "pending root compact memory {} does not match prepared memory {}",
            existing.compact_id, memory.compact_id
        )));
    }
    Ok(Some(*next_open_index))
}

fn apply_root_epoch_boundary(
    parse_stack: &mut ParseStack,
    boundary_idx: usize,
    boundary: Symbol,
    root_epoch: RootEpoch,
) {
    match boundary {
        Symbol::Control(ControlSymbol::Init(_)) => {
            parse_stack.symbols.truncate(boundary_idx + 1);
            parse_stack
                .symbols
                .push(Symbol::RootEpoches(vec![root_epoch]));
        }
        Symbol::RootEpoches(mut root_epochs) => {
            parse_stack.symbols.truncate(boundary_idx);
            root_epochs.push(root_epoch);
            parse_stack.symbols.push(Symbol::RootEpoches(root_epochs));
        }
        _ => unreachable!("root epoch boundary was checked before apply"),
    }
}

fn pending_compact_memory(parse_stack: &ParseStack) -> Option<&MemoryRef> {
    match parse_stack.symbols.last() {
        Some(Symbol::Control(ControlSymbol::Compact(memory, ..))) => Some(memory),
        _ => None,
    }
}

fn validate_pending_compact_memory(
    existing: &MemoryRef,
    memory: &MemoryRef,
) -> Result<(), SpineError> {
    if existing != memory {
        return Err(SpineError::InvalidEvent(format!(
            "pending root compact memory {} does not match prepared memory {}",
            existing.compact_id, memory.compact_id
        )));
    }
    Ok(())
}
