use crate::spine::SpineError;
use crate::spine::archive::SpineArchive;
use crate::spine::archive::tree_meta;
use crate::spine::archive::tree_meta_with_token_baselines;
use crate::spine::model::ContextBaselineSource;
use crate::spine::model::MemoryRef;
use crate::spine::model::NodeId;
use crate::spine::model::SegRef;
use crate::spine::model::SpineLedgerEvent;
use crate::spine::model::SpineToken;
use crate::spine::model::ToolCallEventSegment;
use crate::spine::model::ToolCallSegment;
use crate::spine::model::ToolCallSegmentKind;
use serde::Deserialize;

#[derive(Clone, Debug)]
pub(in crate::spine) struct LexedTokenBatch {
    pub(in crate::spine) events: Vec<SpineLedgerEvent>,
    pub(in crate::spine) tokens: Vec<SpineToken>,
}

impl LexedTokenBatch {
    pub(in crate::spine) fn single(event: SpineLedgerEvent, token: SpineToken) -> Self {
        Self {
            events: vec![event],
            tokens: vec![token],
        }
    }

    pub(in crate::spine) fn into_single(
        self,
        label: &str,
    ) -> Result<(SpineLedgerEvent, SpineToken), SpineError> {
        let mut events = self.events.into_iter();
        let event = events
            .next()
            .ok_or_else(|| SpineError::Invariant(format!("{label} lexer produced no event")))?;
        if events.next().is_some() {
            return Err(SpineError::Invariant(format!(
                "{label} lexer produced multiple events"
            )));
        }
        let mut tokens = self.tokens.into_iter();
        let token = tokens
            .next()
            .ok_or_else(|| SpineError::Invariant(format!("{label} lexer produced no token")))?;
        if tokens.next().is_some() {
            return Err(SpineError::Invariant(format!(
                "{label} lexer produced multiple tokens"
            )));
        }
        Ok((event, token))
    }

    pub(in crate::spine) fn into_single_token(self, label: &str) -> Result<SpineToken, SpineError> {
        self.into_single(label).map(|(_, token)| token)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::spine) enum ControlIntent {
    Ordinary,
    Open,
    Close,
    Next,
}

impl ControlIntent {
    pub(in crate::spine) fn token_sequence(self) -> &'static [LexedTokenKind] {
        match self {
            Self::Ordinary => &[LexedTokenKind::ToolCall],
            Self::Open => &[LexedTokenKind::Open, LexedTokenKind::ToolCall],
            Self::Close => &[LexedTokenKind::Close, LexedTokenKind::ToolCall],
            Self::Next => &[
                LexedTokenKind::Close,
                LexedTokenKind::Open,
                LexedTokenKind::ToolCall,
            ],
        }
    }

    pub(in crate::spine) fn is_close_like(self) -> bool {
        matches!(self, Self::Close | Self::Next)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::spine) enum LexedTokenKind {
    Compact,
    Open,
    Close,
    ToolCall,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::spine) struct ControlToolCallPlan {
    intent: ControlIntent,
    token_sequence: &'static [LexedTokenKind],
}

impl ControlToolCallPlan {
    pub(in crate::spine) fn intent(self) -> ControlIntent {
        self.intent
    }

    pub(in crate::spine) fn token_sequence(self) -> &'static [LexedTokenKind] {
        self.token_sequence
    }

    pub(in crate::spine) fn is_close_like(self) -> bool {
        self.intent.is_close_like()
    }
}

pub(in crate::spine) fn plan_control_toolcall(intent: ControlIntent) -> ControlToolCallPlan {
    ControlToolCallPlan {
        intent,
        token_sequence: intent.token_sequence(),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::spine) struct RootCompactPlan {
    token_sequence: &'static [LexedTokenKind],
}

impl RootCompactPlan {
    pub(in crate::spine) fn token_sequence(self) -> &'static [LexedTokenKind] {
        self.token_sequence
    }

    pub(in crate::spine) fn lex_compact_token(
        self,
        memory: MemoryRef,
        next_open_index: usize,
        next_open_input_tokens: Option<i64>,
        next_open_context_tokens: Option<i64>,
    ) -> Result<SpineToken, SpineError> {
        debug_assert_eq!(
            self.token_sequence(),
            &[LexedTokenKind::Compact, LexedTokenKind::Open]
        );
        lex_root_compact_token(
            memory,
            next_open_index,
            next_open_input_tokens,
            next_open_context_tokens,
        )
    }

    pub(in crate::spine) fn lex_event_token(
        self,
        node: NodeId,
        boundary: u64,
        memory: MemoryRef,
        next_open_index: usize,
        raw_live_hash: String,
        next_open_input_tokens: Option<i64>,
        next_open_context_tokens: Option<i64>,
    ) -> Result<(SpineLedgerEvent, SpineToken), SpineError> {
        debug_assert_eq!(
            self.token_sequence(),
            &[LexedTokenKind::Compact, LexedTokenKind::Open]
        );
        lex_root_compact_event_token(
            node,
            boundary,
            memory,
            next_open_index,
            raw_live_hash,
            next_open_input_tokens,
            next_open_context_tokens,
        )
    }
}

pub(in crate::spine) fn plan_root_compact() -> RootCompactPlan {
    RootCompactPlan {
        token_sequence: &[LexedTokenKind::Compact, LexedTokenKind::Open],
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::spine) enum ParsedControlToolIntent {
    Open { summary: String },
    Close { memory: String },
    Next { summary: String, memory: String },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OpenToolArgs {
    summary: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CloseToolArgs {
    memory: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NextToolArgs {
    summary: String,
    memory: String,
}

pub(in crate::spine) fn parse_control_tool_intent(
    tool_name: &str,
    arguments: &str,
) -> Result<Option<ParsedControlToolIntent>, SpineError> {
    match tool_name {
        "open" => {
            let args: OpenToolArgs = serde_json::from_str(arguments).map_err(|err| {
                SpineError::ToolUse(format!("failed to parse spine.open arguments: {err}"))
            })?;
            Ok(Some(ParsedControlToolIntent::Open {
                summary: args.summary,
            }))
        }
        "close" => {
            let args: CloseToolArgs = serde_json::from_str(arguments).map_err(|err| {
                SpineError::ToolUse(format!("failed to parse spine.close arguments: {err}"))
            })?;
            Ok(Some(ParsedControlToolIntent::Close {
                memory: args.memory.trim().to_string(),
            }))
        }
        "next" => {
            let args: NextToolArgs = serde_json::from_str(arguments).map_err(|err| {
                SpineError::ToolUse(format!("failed to parse spine.next arguments: {err}"))
            })?;
            Ok(Some(ParsedControlToolIntent::Next {
                summary: args.summary,
                memory: args.memory.trim().to_string(),
            }))
        }
        _ => Ok(None),
    }
}

pub(in crate::spine) fn lex_init(
    archive: &SpineArchive,
    raw_start: u64,
) -> Result<LexedTokenBatch, SpineError> {
    Ok(LexedTokenBatch::single(
        SpineLedgerEvent::Init { raw_start },
        SpineToken::Init {
            meta: tree_meta(
                archive,
                NodeId::root_epoch(1),
                raw_start,
                "root".to_string(),
            )?,
        },
    ))
}

pub(in crate::spine) fn lex_init_event_token(
    archive: &SpineArchive,
    raw_start: u64,
) -> Result<(SpineLedgerEvent, SpineToken), SpineError> {
    lex_init(archive, raw_start)?.into_single("init")
}

pub(in crate::spine) fn lex_init_token(
    archive: &SpineArchive,
    raw_start: u64,
) -> Result<SpineToken, SpineError> {
    lex_init(archive, raw_start)?.into_single_token("init")
}

pub(in crate::spine) fn lex_msg(
    raw_ordinal: u64,
    context_index: u64,
    from_user: bool,
    user_anchor: Option<u64>,
) -> Result<LexedTokenBatch, SpineError> {
    let context_index_usize = usize::try_from(context_index)
        .map_err(|_| SpineError::InvalidEvent("context index overflow".to_string()))?;
    Ok(LexedTokenBatch::single(
        SpineLedgerEvent::Msg {
            raw_ordinal,
            context_index,
            from_user,
            user_anchor,
        },
        SpineToken::Msg {
            seg: SegRef::ResponseItem {
                raw_ordinal,
                context_index: context_index_usize,
            },
            from_user,
            user_anchor,
        },
    ))
}

pub(in crate::spine) fn lex_open(
    archive: &SpineArchive,
    child: NodeId,
    boundary: u64,
    index: u64,
    summary: String,
    open_input_tokens: Option<i64>,
    open_context_tokens: Option<i64>,
    open_context_source: Option<ContextBaselineSource>,
) -> Result<LexedTokenBatch, SpineError> {
    if open_input_tokens != open_context_tokens {
        return Err(SpineError::InvalidEvent(format!(
            "open event for node {child} has mismatched provider input baseline encoding"
        )));
    }
    Ok(LexedTokenBatch::single(
        SpineLedgerEvent::Open {
            child: child.clone(),
            boundary,
            index,
            summary: summary.clone(),
            open_input_tokens,
            open_context_tokens,
            open_context_source,
        },
        SpineToken::Open {
            meta: tree_meta_with_token_baselines(
                archive,
                child,
                index,
                summary,
                open_input_tokens,
                open_context_source,
            )?,
        },
    ))
}

pub(in crate::spine) fn lex_open_event_token(
    archive: &SpineArchive,
    child: NodeId,
    boundary: u64,
    index: u64,
    summary: String,
    open_input_tokens: Option<i64>,
    open_context_tokens: Option<i64>,
    open_context_source: Option<ContextBaselineSource>,
) -> Result<(SpineLedgerEvent, SpineToken), SpineError> {
    lex_open(
        archive,
        child,
        boundary,
        index,
        summary,
        open_input_tokens,
        open_context_tokens,
        open_context_source,
    )?
    .into_single("open")
}

pub(in crate::spine) fn lex_open_token(
    archive: &SpineArchive,
    child: NodeId,
    boundary: u64,
    index: u64,
    summary: String,
    open_input_tokens: Option<i64>,
    open_context_tokens: Option<i64>,
    open_context_source: Option<ContextBaselineSource>,
) -> Result<SpineToken, SpineError> {
    lex_open(
        archive,
        child,
        boundary,
        index,
        summary,
        open_input_tokens,
        open_context_tokens,
        open_context_source,
    )?
    .into_single_token("open")
}

pub(in crate::spine) fn lex_close(
    node: NodeId,
    boundary: u64,
    summary: String,
    close_input_tokens: Option<i64>,
    close_context_tokens: Option<i64>,
    memory: MemoryRef,
) -> Result<LexedTokenBatch, SpineError> {
    Ok(LexedTokenBatch::single(
        SpineLedgerEvent::Close {
            node,
            boundary,
            summary,
            close_input_tokens,
            close_context_tokens,
        },
        lex_close_token(memory)?,
    ))
}

pub(in crate::spine) fn lex_close_event_token(
    node: NodeId,
    boundary: u64,
    summary: String,
    close_input_tokens: Option<i64>,
    close_context_tokens: Option<i64>,
    memory: MemoryRef,
) -> Result<(SpineLedgerEvent, SpineToken), SpineError> {
    lex_close(
        node,
        boundary,
        summary,
        close_input_tokens,
        close_context_tokens,
        memory,
    )?
    .into_single("close")
}

pub(in crate::spine) fn lex_close_token(memory: MemoryRef) -> Result<SpineToken, SpineError> {
    Ok(SpineToken::Close { memory })
}

pub(in crate::spine) fn lex_root_compact(
    node: NodeId,
    boundary: u64,
    memory: MemoryRef,
    next_open_index: usize,
    raw_live_hash: String,
    next_open_input_tokens: Option<i64>,
    next_open_context_tokens: Option<i64>,
) -> Result<LexedTokenBatch, SpineError> {
    let next_open_index_u64 = u64::try_from(next_open_index)
        .map_err(|_| SpineError::InvalidEvent("root open index overflow".to_string()))?;
    Ok(LexedTokenBatch::single(
        SpineLedgerEvent::RootCompact {
            node,
            boundary,
            mem: memory.compact_id.clone(),
            next_open_index: next_open_index_u64,
            raw_live_hash,
            next_open_input_tokens,
            next_open_context_tokens,
        },
        lex_root_compact_token(
            memory,
            next_open_index,
            next_open_input_tokens,
            next_open_context_tokens,
        )?,
    ))
}

pub(in crate::spine) fn lex_root_compact_event_token(
    node: NodeId,
    boundary: u64,
    memory: MemoryRef,
    next_open_index: usize,
    raw_live_hash: String,
    next_open_input_tokens: Option<i64>,
    next_open_context_tokens: Option<i64>,
) -> Result<(SpineLedgerEvent, SpineToken), SpineError> {
    lex_root_compact(
        node,
        boundary,
        memory,
        next_open_index,
        raw_live_hash,
        next_open_input_tokens,
        next_open_context_tokens,
    )?
    .into_single("root compact")
}

pub(in crate::spine) fn lex_root_compact_token(
    memory: MemoryRef,
    next_open_index: usize,
    next_open_input_tokens: Option<i64>,
    next_open_context_tokens: Option<i64>,
) -> Result<SpineToken, SpineError> {
    Ok(SpineToken::Compact {
        memory,
        next_open_index,
        next_open_input_tokens,
        next_open_context_tokens,
    })
}

#[derive(Clone, Copy, Debug)]
pub(in crate::spine) struct ToolCallLexSegment {
    pub(in crate::spine) kind: ToolCallSegmentKind,
    pub(in crate::spine) raw_ordinal: u64,
    pub(in crate::spine) context_index: usize,
}

pub(in crate::spine) fn lex_toolcall(
    segments: impl IntoIterator<Item = ToolCallLexSegment>,
    request_call_id_count: Option<usize>,
) -> Result<LexedTokenBatch, SpineError> {
    let segments = segments.into_iter().collect::<Vec<_>>();
    validate_toolcall_segments(&segments, request_call_id_count)?;

    let token_segments = segments
        .iter()
        .map(|segment| ToolCallSegment {
            kind: segment.kind,
            seg: SegRef::ResponseItem {
                raw_ordinal: segment.raw_ordinal,
                context_index: segment.context_index,
            },
        })
        .collect::<Vec<_>>();
    let event_segments = segments
        .iter()
        .map(|segment| {
            Ok(ToolCallEventSegment {
                kind: segment.kind,
                raw_ordinal: segment.raw_ordinal,
                context_index: u64::try_from(segment.context_index).map_err(|_| {
                    SpineError::InvalidEvent("toolcall context index overflow".to_string())
                })?,
            })
        })
        .collect::<Result<Vec<_>, SpineError>>()?;

    Ok(LexedTokenBatch::single(
        SpineLedgerEvent::ToolCall {
            segments: event_segments,
        },
        SpineToken::ToolCall {
            segments: token_segments,
        },
    ))
}

pub(in crate::spine) fn lex_toolcall_event(
    segments: impl IntoIterator<Item = ToolCallEventSegment>,
) -> Result<LexedTokenBatch, SpineError> {
    let segments = segments
        .into_iter()
        .map(|segment| {
            let context_index = usize::try_from(segment.context_index)
                .map_err(|_| SpineError::InvalidEvent("context index overflow".to_string()))?;
            Ok(ToolCallLexSegment {
                kind: segment.kind,
                raw_ordinal: segment.raw_ordinal,
                context_index,
            })
        })
        .collect::<Result<Vec<_>, SpineError>>()?;
    lex_toolcall(segments, None)
}

fn validate_toolcall_segments(
    segments: &[ToolCallLexSegment],
    request_call_id_count: Option<usize>,
) -> Result<(), SpineError> {
    if segments.is_empty() {
        return Err(SpineError::InvalidEvent(
            "completed toolcall must contain at least one segment".to_string(),
        ));
    }
    let mut has_request = false;
    let mut has_response = false;
    let mut previous_context_index = None;
    let mut previous_raw_ordinal = None;
    for (index, segment) in segments.iter().enumerate() {
        match segment.kind {
            ToolCallSegmentKind::Request => {
                if has_response {
                    return Err(SpineError::InvalidEvent(format!(
                        "completed toolcall request segment {index} appears after a response segment"
                    )));
                }
                has_request = true;
            }
            ToolCallSegmentKind::Response => has_response = true,
        }
        if let Some(previous) = previous_context_index {
            if segment.context_index <= previous {
                return Err(SpineError::InvalidEvent(format!(
                    "completed toolcall segment {index} context_index {} is not strictly after previous context_index {previous}",
                    segment.context_index
                )));
            }
        }
        if let Some(previous) = previous_raw_ordinal {
            if segment.raw_ordinal <= previous {
                return Err(SpineError::InvalidEvent(format!(
                    "completed toolcall segment {index} raw_ordinal {} is not strictly after previous raw_ordinal {previous}",
                    segment.raw_ordinal
                )));
            }
        }
        previous_context_index = Some(segment.context_index);
        previous_raw_ordinal = Some(segment.raw_ordinal);
    }
    if !has_request {
        return Err(SpineError::InvalidEvent(
            "completed toolcall must include at least one request segment".to_string(),
        ));
    }
    if !has_response {
        return Err(SpineError::InvalidEvent(
            "completed toolcall must include at least one response segment".to_string(),
        ));
    }
    let request_segment_count = segments
        .iter()
        .filter(|segment| segment.kind == ToolCallSegmentKind::Request)
        .count();
    if let Some(request_call_id_count) = request_call_id_count
        && request_segment_count != request_call_id_count
    {
        return Err(SpineError::InvalidEvent(format!(
            "completed toolcall request segment count {request_segment_count} does not match request call id count {request_call_id_count}",
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn control_intent_token_sequences_match_formular_def() {
        assert_eq!(
            plan_control_toolcall(ControlIntent::Ordinary).token_sequence(),
            &[LexedTokenKind::ToolCall]
        );
        assert_eq!(
            plan_control_toolcall(ControlIntent::Open).token_sequence(),
            &[LexedTokenKind::Open, LexedTokenKind::ToolCall]
        );
        assert_eq!(
            plan_control_toolcall(ControlIntent::Close).token_sequence(),
            &[LexedTokenKind::Close, LexedTokenKind::ToolCall]
        );
        assert_eq!(
            plan_control_toolcall(ControlIntent::Next).token_sequence(),
            &[
                LexedTokenKind::Close,
                LexedTokenKind::Open,
                LexedTokenKind::ToolCall,
            ]
        );
    }

    #[test]
    fn control_toolcall_plan_exposes_intent_and_close_like_classification() {
        assert_eq!(
            plan_control_toolcall(ControlIntent::Open).intent(),
            ControlIntent::Open
        );
        assert!(!plan_control_toolcall(ControlIntent::Ordinary).is_close_like());
        assert!(!plan_control_toolcall(ControlIntent::Open).is_close_like());
        assert!(plan_control_toolcall(ControlIntent::Close).is_close_like());
        assert!(plan_control_toolcall(ControlIntent::Next).is_close_like());
    }

    #[test]
    fn root_compact_plan_matches_formular_def_macro_shape() {
        assert_eq!(
            plan_root_compact().token_sequence(),
            &[LexedTokenKind::Compact, LexedTokenKind::Open]
        );
    }

    #[test]
    fn parse_control_tool_intent_decodes_open_close_and_next_args() {
        assert_eq!(
            parse_control_tool_intent("open", r#"{"summary":"child"}"#).expect("open parses"),
            Some(ParsedControlToolIntent::Open {
                summary: "child".to_string(),
            })
        );
        assert_eq!(
            parse_control_tool_intent("close", r#"{"memory":"  done  "}"#).expect("close parses"),
            Some(ParsedControlToolIntent::Close {
                memory: "done".to_string(),
            })
        );
        assert_eq!(
            parse_control_tool_intent("next", r#"{"summary":"next","memory":"  handoff  "}"#)
                .expect("next parses"),
            Some(ParsedControlToolIntent::Next {
                summary: "next".to_string(),
                memory: "handoff".to_string(),
            })
        );
    }

    #[test]
    fn parse_control_tool_intent_ignores_non_control_tool() {
        assert_eq!(
            parse_control_tool_intent("tree", "{}").expect("tree is non-control"),
            None
        );
    }

    #[test]
    fn parse_control_tool_intent_rejects_unknown_fields() {
        let err = parse_control_tool_intent("close", r#"{"memory":"done","extra":"not allowed"}"#)
            .expect_err("unknown fields are rejected");

        assert!(matches!(
            err,
            SpineError::ToolUse(message)
                if message.contains("failed to parse spine.close arguments")
        ));
    }

    #[test]
    fn lex_msg_produces_matching_event_and_token() {
        let lexed = lex_msg(7, 3, true, Some(2)).expect("msg lexes");

        assert_eq!(lexed.events.len(), 1);
        assert!(matches!(
            lexed.events.first(),
            Some(SpineLedgerEvent::Msg {
                raw_ordinal: 7,
                context_index: 3,
                from_user: true,
                user_anchor: Some(2),
            })
        ));
        assert_eq!(
            lexed.tokens,
            vec![SpineToken::Msg {
                seg: SegRef::ResponseItem {
                    raw_ordinal: 7,
                    context_index: 3,
                },
                from_user: true,
                user_anchor: Some(2),
            }]
        );
    }

    #[test]
    fn lex_init_produces_matching_event_and_token() {
        let archive = SpineArchive::new(PathBuf::from("/tmp/spine-lexer-test"));
        let lexed = lex_init(&archive, 4).expect("init lexes");

        assert_eq!(lexed.events.len(), 1);
        assert!(matches!(
            lexed.events.first(),
            Some(SpineLedgerEvent::Init { raw_start: 4 })
        ));
        assert_eq!(
            lexed.tokens,
            vec![SpineToken::Init {
                meta: crate::spine::model::TreeMeta {
                    id: NodeId::root_epoch(1),
                    index: 4,
                    summary: "root".to_string(),
                    open_input_tokens: None,
                    open_context_tokens: None,
                    open_context_source: None,
                    node_dir: PathBuf::from("/tmp/spine-lexer-test/nodes/1"),
                },
            }]
        );
    }

    #[test]
    fn lex_open_produces_matching_event_and_token() {
        let archive = SpineArchive::new(PathBuf::from("/tmp/spine-lexer-test"));
        let child = NodeId::root_epoch(1).child(2);
        let lexed = lex_open(
            &archive,
            child.clone(),
            11,
            7,
            "child summary".to_string(),
            Some(123),
            Some(123),
            Some(ContextBaselineSource::ProviderAtOpen),
        )
        .expect("open lexes");

        assert_eq!(lexed.events.len(), 1);
        match lexed.events.first() {
            Some(SpineLedgerEvent::Open {
                child: event_child,
                boundary,
                index,
                summary,
                open_input_tokens,
                open_context_tokens,
                open_context_source,
            }) => {
                assert_eq!(event_child, &child);
                assert_eq!(*boundary, 11);
                assert_eq!(*index, 7);
                assert_eq!(summary, "child summary");
                assert_eq!(*open_input_tokens, Some(123));
                assert_eq!(*open_context_tokens, Some(123));
                assert_eq!(
                    *open_context_source,
                    Some(ContextBaselineSource::ProviderAtOpen)
                );
            }
            other => panic!("unexpected open event: {other:?}"),
        }

        assert_eq!(
            lexed.tokens,
            vec![SpineToken::Open {
                meta: crate::spine::model::TreeMeta {
                    id: child,
                    index: 7,
                    summary: "child summary".to_string(),
                    open_input_tokens: Some(123),
                    open_context_tokens: Some(123),
                    open_context_source: Some(ContextBaselineSource::ProviderAtOpen),
                    node_dir: PathBuf::from("/tmp/spine-lexer-test/nodes/1/2"),
                },
            }]
        );
    }

    #[test]
    fn lex_open_rejects_mismatched_provider_baselines() {
        let archive = SpineArchive::new(PathBuf::from("/tmp/spine-lexer-test"));
        let err = lex_open(
            &archive,
            NodeId::root_epoch(1).child(2),
            11,
            7,
            "child summary".to_string(),
            Some(123),
            Some(122),
            Some(ContextBaselineSource::ProviderAtOpen),
        )
        .expect_err("mismatched baselines are rejected");

        assert!(matches!(
            err,
            SpineError::InvalidEvent(message)
                if message.contains("mismatched provider input baseline encoding")
        ));
    }

    #[test]
    fn lex_close_produces_matching_event_and_token() {
        let node = NodeId::root_epoch(1).child(2);
        let memory = MemoryRef {
            compact_id: "close-1-2".to_string(),
            node_id: node.clone(),
            body_path: PathBuf::from("/tmp/spine-lexer-test/body/close-1-2.md"),
            body_hash: "closehash".to_string(),
            source_raw_range: 2..8,
            source_context_range: 3..5,
            source_token_seq: 11..12,
            open_input_tokens: Some(100),
            close_input_tokens: Some(150),
            open_context_tokens: Some(100),
            close_context_tokens: Some(150),
            closed_source_suffix_tokens: None,
            closed_memory_context_tokens: None,
            open_context_source: Some(ContextBaselineSource::ProviderAtOpen),
            memory_output_tokens: None,
        };

        let lexed = lex_close(
            node.clone(),
            8,
            "closed node".to_string(),
            Some(150),
            Some(150),
            memory.clone(),
        )
        .expect("close lexes");

        assert_eq!(lexed.events.len(), 1);
        assert!(matches!(
            lexed.events.first(),
            Some(SpineLedgerEvent::Close {
                node: event_node,
                boundary: 8,
                summary,
                close_input_tokens: Some(150),
                close_context_tokens: Some(150),
            }) if event_node == &node && summary == "closed node"
        ));
        assert_eq!(lexed.tokens, vec![SpineToken::Close { memory }]);
    }

    #[test]
    fn lex_root_compact_produces_matching_event_and_token() {
        let node = NodeId::root_epoch(2);
        let memory = MemoryRef {
            compact_id: "root-2-9".to_string(),
            node_id: node.clone(),
            body_path: PathBuf::from("/tmp/spine-lexer-test/body/root-2-9.md"),
            body_hash: "abc123".to_string(),
            source_raw_range: 0..9,
            source_context_range: 0..4,
            source_token_seq: 5..6,
            open_input_tokens: None,
            close_input_tokens: Some(900),
            open_context_tokens: None,
            close_context_tokens: Some(800),
            closed_source_suffix_tokens: None,
            closed_memory_context_tokens: None,
            open_context_source: None,
            memory_output_tokens: None,
        };

        let lexed = lex_root_compact(
            node.clone(),
            9,
            memory.clone(),
            3,
            "raw-live-hash".to_string(),
            Some(111),
            Some(222),
        )
        .expect("root compact lexes");

        assert_eq!(lexed.events.len(), 1);
        assert!(matches!(
            lexed.events.first(),
            Some(SpineLedgerEvent::RootCompact {
                node: event_node,
                boundary: 9,
                mem,
                next_open_index: 3,
                raw_live_hash,
                next_open_input_tokens: Some(111),
                next_open_context_tokens: Some(222),
            }) if event_node == &node
                && mem == "root-2-9"
                && raw_live_hash == "raw-live-hash"
        ));
        assert_eq!(
            lexed.tokens,
            vec![SpineToken::Compact {
                memory,
                next_open_index: 3,
                next_open_input_tokens: Some(111),
                next_open_context_tokens: Some(222),
            }]
        );
    }

    #[test]
    fn lex_toolcall_produces_matching_event_and_token() {
        let lexed = lex_toolcall(
            [
                ToolCallLexSegment {
                    kind: ToolCallSegmentKind::Request,
                    raw_ordinal: 10,
                    context_index: 4,
                },
                ToolCallLexSegment {
                    kind: ToolCallSegmentKind::Response,
                    raw_ordinal: 11,
                    context_index: 5,
                },
                ToolCallLexSegment {
                    kind: ToolCallSegmentKind::Response,
                    raw_ordinal: 12,
                    context_index: 6,
                },
            ],
            Some(1),
        )
        .expect("toolcall lexes");

        assert_eq!(lexed.events.len(), 1);
        match lexed.events.first() {
            Some(SpineLedgerEvent::ToolCall { segments }) => assert_eq!(
                segments,
                &vec![
                    ToolCallEventSegment {
                        kind: ToolCallSegmentKind::Request,
                        raw_ordinal: 10,
                        context_index: 4,
                    },
                    ToolCallEventSegment {
                        kind: ToolCallSegmentKind::Response,
                        raw_ordinal: 11,
                        context_index: 5,
                    },
                    ToolCallEventSegment {
                        kind: ToolCallSegmentKind::Response,
                        raw_ordinal: 12,
                        context_index: 6,
                    },
                ]
            ),
            other => panic!("unexpected toolcall event: {other:?}"),
        }
        assert_eq!(
            lexed.tokens,
            vec![SpineToken::ToolCall {
                segments: vec![
                    ToolCallSegment {
                        kind: ToolCallSegmentKind::Request,
                        seg: SegRef::ResponseItem {
                            raw_ordinal: 10,
                            context_index: 4,
                        },
                    },
                    ToolCallSegment {
                        kind: ToolCallSegmentKind::Response,
                        seg: SegRef::ResponseItem {
                            raw_ordinal: 11,
                            context_index: 5,
                        },
                    },
                    ToolCallSegment {
                        kind: ToolCallSegmentKind::Response,
                        seg: SegRef::ResponseItem {
                            raw_ordinal: 12,
                            context_index: 6,
                        },
                    },
                ],
            }]
        );
    }

    #[test]
    fn lex_toolcall_rejects_request_after_response() {
        let err = lex_toolcall(
            [
                ToolCallLexSegment {
                    kind: ToolCallSegmentKind::Request,
                    raw_ordinal: 10,
                    context_index: 4,
                },
                ToolCallLexSegment {
                    kind: ToolCallSegmentKind::Response,
                    raw_ordinal: 11,
                    context_index: 5,
                },
                ToolCallLexSegment {
                    kind: ToolCallSegmentKind::Request,
                    raw_ordinal: 12,
                    context_index: 6,
                },
            ],
            Some(2),
        )
        .expect_err("invalid order is rejected");

        assert!(
            matches!(err, SpineError::InvalidEvent(message) if message.contains("appears after a response"))
        );
    }
}
