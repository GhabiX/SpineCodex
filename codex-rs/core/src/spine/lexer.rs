use crate::spine::SpineError;
use crate::spine::model::SegRef;
use crate::spine::model::SpineLedgerEvent;
use crate::spine::model::SpineToken;
use crate::spine::model::ToolCallEventSegment;
use crate::spine::model::ToolCallSegment;
use crate::spine::model::ToolCallSegmentKind;

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
