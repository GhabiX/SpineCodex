mod archive;
mod checkpoint;
mod instructions;
mod io;
mod model;
mod parse_stack;
mod render;
mod runtime;
mod store;

use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;

pub(crate) use runtime::SPINE_NAMESPACE;
pub(crate) use runtime::SPINE_TOOL_CLOSE;
pub(crate) use runtime::SPINE_TOOL_OPEN;
pub(crate) use runtime::SPINE_TOOL_TREE;
pub(crate) use runtime::SpineCloseCompact;
pub(crate) use runtime::SpineCommitKind;
pub(crate) use runtime::SpineError;
pub(crate) use runtime::SpinePendingCommit;
pub(crate) use runtime::SpineRuntime;
pub(crate) use runtime::SpineSessionState;
pub(crate) use runtime::is_user_message;
pub(crate) use store::SpineStore;

pub(crate) use instructions::append_spine_view_instructions;

const CHECKPOINT_VERSION: u32 = 1;

pub(crate) const ROOT_EPOCH_COMPACT_DIRECTIVE: &str = "\
You are compacting a Spine root epoch after native context compaction.
The memory you produce is runtime-generated root-epoch memory. Treat it like
a normal Spine task memory, except this fold is triggered by native compact
rather than `spine.close`.

Preserve factual Spine control events visible in the transcript, including
successful `spine.open`, `spine.close`, and `spine.tree` calls, plus existing
`<spine_memory runtime_generated=\"true\">` memory snippets. Do not claim the
`spine` namespace or Spine tools are unavailable when the transcript contains
successful Spine function calls.";

pub(crate) fn root_epoch_compact_directive_message() -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: "system".to_string(),
        content: vec![ContentItem::InputText {
            text: ROOT_EPOCH_COMPACT_DIRECTIVE.to_string(),
        }],
        phase: None,
    }
}
