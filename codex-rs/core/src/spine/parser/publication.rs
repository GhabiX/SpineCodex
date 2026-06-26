use codex_protocol::models::ResponseItem;

use super::materialize_parse_stack_variable_context;
use crate::spine::SpineError;
use crate::spine::model::ToolCallSegmentKind;
use crate::spine::model::TrimProjection;
use crate::spine::parse_stack::ParseStack;

#[derive(Clone, Debug, PartialEq)]
pub(in crate::spine) struct ParserPublicationPlan {
    operation: &'static str,
    suffix_start: usize,
    replacement_prefix: Vec<ResponseItem>,
    preserve_host_history_from: usize,
    append_current_tool_response_if_missing: bool,
    atomic_mutable_context_segments: Vec<ParserPublicationToolcallSegment>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ParserPublicationToolcallSegment {
    pub(super) kind: ToolCallSegmentKind,
    pub(super) mutable_context_index: usize,
}

pub(super) type ParserPublicationToolcallSegmentEvidence = (ToolCallSegmentKind, usize);

#[derive(Clone, Debug, PartialEq)]
pub(in crate::spine) struct ParserPublicationUpdate {
    operation: &'static str,
    suffix_start: usize,
    expected_history: Vec<ResponseItem>,
    replacement: Vec<ResponseItem>,
}

impl ParserPublicationUpdate {
    fn new(
        operation: &'static str,
        suffix_start: usize,
        expected_history: Vec<ResponseItem>,
        replacement: Vec<ResponseItem>,
    ) -> Self {
        Self {
            operation,
            suffix_start,
            expected_history,
            replacement,
        }
    }

    pub(in crate::spine) fn into_host_history_update<T>(
        self,
        call_id: &str,
        build_update: impl FnOnce(&str, &'static str, usize, Vec<ResponseItem>, Vec<ResponseItem>) -> T,
    ) -> T {
        build_update(
            call_id,
            self.operation,
            self.suffix_start,
            self.expected_history,
            self.replacement,
        )
    }
}

impl ParserPublicationPlan {
    pub(super) fn new(
        operation: &'static str,
        suffix_start: usize,
        replacement_prefix: Vec<ResponseItem>,
        preserve_host_history_from: usize,
        append_current_tool_response_if_missing: bool,
        atomic_mutable_context_segments: impl IntoIterator<
            Item = ParserPublicationToolcallSegmentEvidence,
        >,
    ) -> Self {
        Self {
            operation,
            suffix_start,
            replacement_prefix,
            preserve_host_history_from,
            append_current_tool_response_if_missing,
            atomic_mutable_context_segments: atomic_mutable_context_segments
                .into_iter()
                .map(
                    |(kind, mutable_context_index)| ParserPublicationToolcallSegment {
                        kind,
                        mutable_context_index,
                    },
                )
                .collect(),
        }
    }

    pub(in crate::spine) fn suffix_start(&self) -> usize {
        self.suffix_start
    }

    pub(in crate::spine) fn preserve_host_history_from(&self) -> usize {
        self.preserve_host_history_from
    }

    pub(in crate::spine) fn publication_update_with_host_boundaries(
        &self,
        call_id: &str,
        tool_resp_item: &ResponseItem,
        tool_resp_already_recorded: bool,
        host_suffix_start: usize,
        host_preserve_history_from: usize,
        history_items: &[ResponseItem],
        full_index_for_mutable_index: impl FnMut(usize) -> Result<usize, SpineError>,
        full_index_for_mutable_boundary: impl FnMut(usize) -> Result<usize, SpineError>,
    ) -> Result<Option<ParserPublicationUpdate>, SpineError> {
        let suffix_end = history_items.len();
        if host_suffix_start > suffix_end {
            return Err(SpineError::Invariant(format!(
                "{} suffix start {} exceeds history length {suffix_end} for call_id={call_id}",
                self.operation, host_suffix_start
            )));
        }
        if host_preserve_history_from > suffix_end {
            return Err(SpineError::Invariant(format!(
                "{} preserve-host-history index {} exceeds history length {suffix_end} for call_id={call_id}",
                self.operation, host_preserve_history_from
            )));
        }
        self.validate_host_boundaries_do_not_split_toolcall(
            host_suffix_start,
            host_preserve_history_from,
            history_items.len(),
            full_index_for_mutable_index,
            full_index_for_mutable_boundary,
        )?;
        let mut replacement = self.replacement_prefix.clone();
        replacement.extend_from_slice(&history_items[host_preserve_history_from..]);
        if self.append_current_tool_response_if_missing && !tool_resp_already_recorded {
            replacement.push(tool_resp_item.clone());
        }
        Ok(Some(ParserPublicationUpdate::new(
            self.operation,
            host_suffix_start,
            history_items.to_vec(),
            replacement,
        )))
    }

    fn validate_host_boundaries_do_not_split_toolcall(
        &self,
        host_suffix_start: usize,
        host_preserve_history_from: usize,
        history_len: usize,
        mut full_index_for_mutable_index: impl FnMut(usize) -> Result<usize, SpineError>,
        mut full_index_for_mutable_boundary: impl FnMut(usize) -> Result<usize, SpineError>,
    ) -> Result<(), SpineError> {
        if self.atomic_mutable_context_segments.is_empty() {
            return Ok(());
        }
        let mut full_start = usize::MAX;
        let mut full_end = 0usize;
        for segment in &self.atomic_mutable_context_segments {
            match segment.kind {
                ToolCallSegmentKind::Request => {
                    let full_index = full_index_for_mutable_index(segment.mutable_context_index)?;
                    full_start = full_start.min(full_index);
                    full_end = full_end.max(full_index.checked_add(1).ok_or_else(|| {
                        SpineError::InvalidEvent("toolcall full host range overflow".to_string())
                    })?);
                }
                ToolCallSegmentKind::Response => {
                    let full_boundary =
                        full_index_for_mutable_boundary(segment.mutable_context_index)?;
                    full_start = full_start.min(full_boundary);
                    let response_end = if full_boundary == history_len {
                        full_boundary
                    } else {
                        full_boundary.checked_add(1).ok_or_else(|| {
                            SpineError::InvalidEvent(
                                "toolcall full host range overflow".to_string(),
                            )
                        })?
                    };
                    full_end = full_end.max(response_end);
                }
            }
        }
        for boundary in [host_suffix_start, host_preserve_history_from] {
            if full_start < boundary && boundary < full_end {
                return Err(SpineError::Invariant(format!(
                    "spine publication boundary {boundary} splits completed toolcall full host range [{full_start}..{full_end})"
                )));
            }
        }
        Ok(())
    }
}

fn full_variable_context_publication_update(
    operation: &'static str,
    variable_context: Vec<ResponseItem>,
    history_items: &[ResponseItem],
) -> Option<ParserPublicationUpdate> {
    if variable_context.as_slice() == history_items {
        return None;
    }
    Some(ParserPublicationUpdate::new(
        operation,
        0,
        history_items.to_vec(),
        variable_context,
    ))
}

pub(super) fn full_variable_context_publication_update_from_parse_stack<T>(
    parse_stack: &ParseStack,
    call_id: &str,
    operation: &'static str,
    raw_items: &[Option<ResponseItem>],
    trim_projection: &TrimProjection,
    history_items: &[ResponseItem],
    build_update: impl FnOnce(&str, &'static str, usize, Vec<ResponseItem>, Vec<ResponseItem>) -> T,
) -> Result<Option<T>, SpineError> {
    let variable_context =
        materialize_parse_stack_variable_context(parse_stack, raw_items, trim_projection)?;
    Ok(full_variable_context_publication_update(
        operation,
        variable_context,
        history_items,
    ))
    .map(|update| update.map(|update| update.into_host_history_update(call_id, build_update)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn completed_toolcall_segments() -> Vec<ParserPublicationToolcallSegment> {
        vec![
            ParserPublicationToolcallSegment {
                kind: ToolCallSegmentKind::Request,
                mutable_context_index: 0,
            },
            ParserPublicationToolcallSegment {
                kind: ToolCallSegmentKind::Response,
                mutable_context_index: 1,
            },
        ]
    }

    fn publication_plan() -> ParserPublicationPlan {
        ParserPublicationPlan {
            operation: "test",
            suffix_start: 1,
            replacement_prefix: Vec::new(),
            preserve_host_history_from: 2,
            append_current_tool_response_if_missing: false,
            atomic_mutable_context_segments: completed_toolcall_segments(),
        }
    }

    fn fixed_prefix_full_index_for_mutable_index(index: usize) -> Result<usize, SpineError> {
        index
            .checked_add(1)
            .ok_or_else(|| SpineError::InvalidEvent("test host index overflow".to_string()))
    }

    fn fixed_prefix_full_index_for_mutable_boundary(index: usize) -> Result<usize, SpineError> {
        fixed_prefix_full_index_for_mutable_index(index)
    }

    fn history_items() -> Vec<ResponseItem> {
        vec![
            ResponseItem::Other,
            ResponseItem::Other,
            ResponseItem::Other,
        ]
    }

    #[test]
    fn publication_plan_rejects_boundary_inside_toolcall() {
        let history_items = history_items();
        let err = publication_plan()
            .publication_update_with_host_boundaries(
                "call",
                &history_items[2],
                true,
                2,
                history_items.len(),
                &history_items,
                fixed_prefix_full_index_for_mutable_index,
                fixed_prefix_full_index_for_mutable_boundary,
            )
            .expect_err("boundary between request and response must be rejected");
        assert!(
            err.to_string().contains("splits completed toolcall"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn publication_plan_accepts_boundaries_at_toolcall_edges() {
        let history_items = history_items();
        publication_plan()
            .publication_update_with_host_boundaries(
                "call",
                &history_items[2],
                true,
                1,
                history_items.len(),
                &history_items,
                fixed_prefix_full_index_for_mutable_index,
                fixed_prefix_full_index_for_mutable_boundary,
            )
            .expect("boundary at toolcall start is valid");
        publication_plan()
            .publication_update_with_host_boundaries(
                "call",
                &history_items[2],
                true,
                0,
                3,
                &history_items,
                fixed_prefix_full_index_for_mutable_index,
                fixed_prefix_full_index_for_mutable_boundary,
            )
            .expect("boundary at toolcall end is valid");
    }
}
