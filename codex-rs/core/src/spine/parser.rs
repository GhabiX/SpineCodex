//! Parser boundary for Spine token consumption and variable context projection.
//!
//! The intended ownership chain is:
//!
//! ```text
//! hook -> lexer -> parser -> PS -> h(PS) -> host publication
//! ```
//!
//! `ParserState` is the production owner of the live parse stack. Runtime code
//! may provide evidence and durable side effects, but parser-visible tokens
//! enter through this facade.

mod publication;
pub(in crate::spine) use publication::ParserPublicationPlan;
pub(in crate::spine) use publication::checkpoint_variable_context_from_parse_stack;
mod reducer;
mod replay;
mod state;
pub(in crate::spine) use state::ParserState;
mod transaction;
pub(in crate::spine) use transaction::ParserCommitInstall;
pub(in crate::spine) use transaction::ParserCommitPreparedInstall;
pub(in crate::spine) use transaction::ParserRootCompactPreparedCommitInstall;
pub(in crate::spine) use transaction::ParserRootCompactPublicationParts;
