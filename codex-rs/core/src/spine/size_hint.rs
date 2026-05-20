use super::ids::NodeId;
use super::runtime::SpineRuntimeError;
use super::runtime::SpineRuntimeHint;
use super::state::SpineState;
use super::store::SpineSidecarStore;
use tracing::warn;

const SPINE_HINT_FIRST_THRESHOLD_TOKENS: u64 = 50_000;
const SPINE_HINT_STEP_TOKENS: u64 = 30_000;

pub(crate) fn size_hint_for_cursor(
    state: &SpineState,
    store: &SpineSidecarStore,
    cursor: &NodeId,
    next_raw_ordinal: u64,
    source: String,
) -> Result<Option<SpineRuntimeHint>, SpineRuntimeError> {
    let node_id = cursor.clone();
    let start = state
        .node(&node_id)
        .and_then(|node| node.raw_start_ordinal)
        .ok_or_else(|| SpineRuntimeError::MissingRawStartOrdinal {
            node_id: node_id.clone(),
        })?;
    let estimated_tokens = store.estimate_raw_response_tokens(start, next_raw_ordinal)?;
    let Some(threshold_tokens) = size_hint_threshold(estimated_tokens) else {
        return Ok(None);
    };
    match store.has_size_hint_emitted(&node_id, threshold_tokens) {
        Ok(true) => return Ok(None),
        Ok(false) => {}
        Err(err) => {
            warn!("failed to read non-semantic Spine size hint cache; skipping hint: {err}");
            return Ok(None);
        }
    }
    if let Err(err) =
        store.append_size_hint_emitted(&node_id, threshold_tokens, estimated_tokens, source)
    {
        warn!("failed to update non-semantic Spine size hint cache; skipping hint: {err}");
        return Ok(None);
    }
    Ok(Some(SpineRuntimeHint {
        node_id,
        estimated_tokens,
        threshold_tokens,
    }))
}

pub(crate) fn size_hint_threshold(estimated_tokens: u64) -> Option<u64> {
    if estimated_tokens < SPINE_HINT_FIRST_THRESHOLD_TOKENS {
        return None;
    }
    let offset = estimated_tokens - SPINE_HINT_FIRST_THRESHOLD_TOKENS;
    let steps = offset / SPINE_HINT_STEP_TOKENS;
    Some(SPINE_HINT_FIRST_THRESHOLD_TOKENS + steps * SPINE_HINT_STEP_TOKENS)
}
