use super::memory::MemKind;
use super::token::NodeId;
use crate::spine::SpineError;
use serde::Deserialize;
use serde::Serialize;
use std::collections::BTreeSet;

pub(in crate::spine) const COMMIT_MARKER_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(in crate::spine) enum SpineCommitKindMarker {
    Close,
    CloseThenOpen,
    RootCompact,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(in crate::spine) struct SpineCommitMemoryRef {
    pub(in crate::spine) compact_id: String,
    pub(in crate::spine) kind: MemKind,
    pub(in crate::spine) node: NodeId,
    pub(in crate::spine) raw_start: u64,
    pub(in crate::spine) raw_end: u64,
    pub(in crate::spine) context_start: usize,
    pub(in crate::spine) context_end: usize,
    #[serde(default)]
    pub(in crate::spine) raw_live_hash: Option<String>,
    pub(in crate::spine) body_path: String,
    pub(in crate::spine) body_hash: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(in crate::spine) struct SpineCommitMarker {
    pub(in crate::spine) version: u32,
    pub(in crate::spine) op_id: String,
    pub(in crate::spine) kind: SpineCommitKindMarker,
    pub(in crate::spine) token_seq_start: u64,
    pub(in crate::spine) token_seq_end: u64,
    pub(in crate::spine) raw_boundary: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(in crate::spine) raw_live_hash: Option<String>,
    pub(in crate::spine) memory_refs: Vec<SpineCommitMemoryRef>,
}

pub(in crate::spine) fn commit_marker_structural_event_seqs(
    marker: &SpineCommitMarker,
) -> Result<BTreeSet<u64>, SpineError> {
    let mut seqs = BTreeSet::new();
    seqs.insert(marker.token_seq_start);
    if marker.kind == SpineCommitKindMarker::CloseThenOpen {
        seqs.insert(marker.token_seq_start.checked_add(1).ok_or_else(|| {
            SpineError::InvalidEvent("Spine commit marker token seq overflow".to_string())
        })?);
    }
    Ok(seqs)
}
