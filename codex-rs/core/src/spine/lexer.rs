use crate::spine::SpineError;
use crate::spine::model::SegRef;
use crate::spine::model::SpineLedgerEvent;
use crate::spine::model::SpineToken;

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
}
