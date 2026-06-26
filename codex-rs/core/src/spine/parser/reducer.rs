use crate::spine::SpineError;
use crate::spine::archive::SpineArchive;
use crate::spine::lexer::LexedTokenBatch;
use crate::spine::parse_stack::ParseStack;

pub(super) fn apply_lexed_batches_to_parse_stack<'a>(
    parse_stack: &mut ParseStack,
    batches: impl IntoIterator<Item = &'a LexedTokenBatch>,
    archive: &SpineArchive,
) -> Result<(), SpineError> {
    for batch in batches {
        for token in batch.tokens.iter().cloned() {
            parse_stack.shift(token, archive)?;
        }
    }
    Ok(())
}
