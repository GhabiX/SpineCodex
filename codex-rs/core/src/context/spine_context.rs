use super::ContextualUserFragment;
use codex_context_fragments::AnnotatedContent;
use codex_context_fragments::set_annotated_content;
use codex_context_fragments::to_annotated_content;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ContentItemKind;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::MULTI_AGENT_MODE_CLOSE_TAG;
use codex_protocol::protocol::MULTI_AGENT_MODE_OPEN_TAG;
use spine_core::host::NodeContextCost;
use spine_core::host::NodeId;
use spine_core::host::NodeStatus;
use spine_core::host::SpawnArrival;
use spine_core::host::SpawnOutcome;
use spine_core::host::SpawnTask;

/// Maximum serialized bytes for one complete Spine-owned provider input value.
///
/// `spine_model_item_wire_bytes` serializes the exact `{"input":[item]}`
/// Responses value, including JSON escaping and structural framing. The shared
/// Spine contract reserves additional provider-created framing tokens so the
/// complete item remains strictly below 10K tokens.
pub(crate) const MAX_SPINE_MODEL_ITEM_WIRE_BYTES: usize =
    spine_core::host::MAX_MODEL_VISIBLE_PROVIDER_VALUE_BYTES;

/// Synthetic fragments leave a 1,000-byte allowance for the enclosing
/// `ResponseItem` JSON representation checked at the final model-item gate.
pub(crate) const MAX_SPINE_FRAGMENT_BYTES: usize = 8_000;

pub(crate) struct SpineMultiAgentModeInstructions(String);

impl SpineMultiAgentModeInstructions {
    pub(crate) fn new(prompt: &str) -> Self {
        Self(prompt.to_string())
    }
}

impl ContextualUserFragment for SpineMultiAgentModeInstructions {
    fn content_kind(&self) -> ContentItemKind {
        ContentItemKind("multi_agent.mode_instructions".to_string())
    }
    fn role(&self) -> &'static str {
        "developer"
    }

    fn markers(&self) -> (&'static str, &'static str) {
        Self::type_markers()
    }

    fn type_markers() -> (&'static str, &'static str) {
        (MULTI_AGENT_MODE_OPEN_TAG, MULTI_AGENT_MODE_CLOSE_TAG)
    }

    fn body(&self) -> String {
        self.0.clone()
    }
}

macro_rules! impl_fragment {
    ($name:ident, $role:literal, $start:literal, $end:literal) => {
        impl ContextualUserFragment for $name {
            fn content_kind(&self) -> ContentItemKind {
                ContentItemKind(concat!("spine.", stringify!($name)).to_string())
            }
            fn role(&self) -> &'static str {
                $role
            }

            fn markers(&self) -> (&'static str, &'static str) {
                Self::type_markers()
            }

            fn body(&self) -> String {
                self.body.clone()
            }

            fn type_markers() -> (&'static str, &'static str) {
                ($start, $end)
            }
        }
    };
}

pub(crate) struct SpineNodeFragment {
    body: String,
}

impl SpineNodeFragment {
    pub(crate) fn new(
        node_id: &NodeId,
        summary: &str,
        status: NodeStatus,
        _context_cost: NodeContextCost,
        prompt: &str,
    ) -> Result<Self, String> {
        let attributes = format!(
            " id=\"{node_id}\" summary=\"{}\" status=\"{}\">",
            escape_xml_attribute(summary),
            status_name(status),
        );
        let body = if matches!(status, NodeStatus::Live | NodeStatus::Opened) {
            let prompt = prompt.trim();
            if prompt.is_empty() {
                format!("{attributes}\n")
            } else {
                format!("{attributes}\n{prompt}\n")
            }
        } else {
            attributes
        };
        checked_fragment("node", "<spine_node", body, "</spine_node>").map(|body| Self { body })
    }
}

impl_fragment!(
    SpineNodeFragment,
    "developer",
    "<spine_node",
    "</spine_node>"
);

pub(crate) struct SpineMemoryFragment {
    body: String,
}

impl SpineMemoryFragment {
    pub(crate) fn new(owner_node: &NodeId, memory: &str) -> Result<Self, String> {
        let body = format!(" node_id=\"{owner_node}\">\n{memory}\n");
        checked_fragment("memory", "<spine_memory", body, "</spine_memory>")
            .map(|body| Self { body })
    }
}

impl_fragment!(
    SpineMemoryFragment,
    "user",
    "<spine_memory",
    "</spine_memory>"
);

pub(crate) struct SpineBirthFragment {
    body: String,
}

impl SpineBirthFragment {
    pub(crate) fn new(owner_node: &NodeId, task: &SpawnTask) -> Result<Self, String> {
        let body = format!(
            " node_id=\"{owner_node}\" summary=\"{}\">\n{}\n",
            escape_xml_attribute(&task.summary),
            task.prompt,
        );
        checked_fragment("birth", "<spine_spawn", body, "</spine_spawn>").map(|body| Self { body })
    }
}

impl_fragment!(SpineBirthFragment, "user", "<spine_spawn", "</spine_spawn>");

/// A spawned child's final result as its parent receives it: a completed
/// child's memory, otherwise its diagnostic.
pub(crate) struct SpineArrivalFragment {
    body: String,
}

impl SpineArrivalFragment {
    pub(crate) fn new(arrival: &SpawnArrival) -> Result<Self, String> {
        let text = match arrival.outcome {
            SpawnOutcome::Completed => arrival.memory.as_deref(),
            SpawnOutcome::Errored | SpawnOutcome::Aborted => arrival.diagnostic.as_deref(),
        }
        .ok_or_else(|| format!("Spine arrival from {} has no text", arrival.producer))?;
        let body = format!(
            " node_id=\"{}\" outcome=\"{}\">\n{text}\n",
            arrival.producer,
            outcome_name(arrival.outcome),
        );
        checked_fragment("arrival", "<spine_arrival", body, "</spine_arrival>")
            .map(|body| Self { body })
    }
}

impl_fragment!(
    SpineArrivalFragment,
    "user",
    "<spine_arrival",
    "</spine_arrival>"
);

pub(crate) struct SpineUserAnchor(u64);

impl SpineUserAnchor {
    pub(crate) fn new(anchor: u64) -> Self {
        Self(anchor)
    }

    pub(crate) fn prepend_to(self, item: &mut ResponseItem) {
        let ResponseItem::Message { role, content, .. } = item else {
            return;
        };
        if role != "user" {
            return;
        }
        let prefix = self.render();
        if let Some(ContentItem::InputText { text }) = content
            .iter_mut()
            .find(|item| matches!(item, ContentItem::InputText { .. }))
        {
            text.insert_str(0, &prefix);
        } else {
            let Some(mut content) = to_annotated_content(item) else {
                unreachable!("the user message variant was checked before inserting its anchor");
            };
            content.insert(0, AnnotatedContent::input_text(prefix, self.content_kind()));
            set_annotated_content(item, content);
        }
    }
}

impl ContextualUserFragment for SpineUserAnchor {
    fn content_kind(&self) -> ContentItemKind {
        ContentItemKind("spine.user_anchor".to_string())
    }
    fn role(&self) -> &'static str {
        "user"
    }

    fn markers(&self) -> (&'static str, &'static str) {
        Self::type_markers()
    }

    fn body(&self) -> String {
        self.0.to_string()
    }

    fn type_markers() -> (&'static str, &'static str) {
        ("[U", "]\n")
    }

    fn matches_text(text: &str) -> bool {
        text.trim()
            .strip_prefix("[U")
            .and_then(|body| body.strip_suffix(']'))
            .is_some_and(|body| body.parse::<u64>().is_ok())
    }
}

pub(crate) fn spine_model_item_wire_bytes(item: &ResponseItem) -> Result<usize, String> {
    serde_json::to_vec(&serde_json::json!({ "input": [item] }))
        .map(|encoded| encoded.len())
        .map_err(|error| format!("failed to serialize Spine provider input item: {error}"))
}

pub(crate) fn validate_spine_model_item(item: &ResponseItem) -> Result<(), String> {
    let wire_bytes = spine_model_item_wire_bytes(item)?;
    if wire_bytes > MAX_SPINE_MODEL_ITEM_WIRE_BYTES {
        return Err(format!(
            "Spine model provider value is {wire_bytes} bytes; maximum is \
             {MAX_SPINE_MODEL_ITEM_WIRE_BYTES}"
        ));
    }
    Ok(())
}

fn checked_fragment(
    kind: &'static str,
    start: &'static str,
    body: String,
    end: &'static str,
) -> Result<String, String> {
    let rendered_bytes = start
        .len()
        .saturating_add(body.len())
        .saturating_add(end.len());
    if rendered_bytes > MAX_SPINE_FRAGMENT_BYTES {
        return Err(format!(
            "Spine {kind} fragment is {rendered_bytes} bytes; maximum is {MAX_SPINE_FRAGMENT_BYTES}"
        ));
    }
    Ok(body)
}

fn status_name(status: NodeStatus) -> &'static str {
    match status {
        NodeStatus::Live => "live",
        NodeStatus::Opened => "opened",
        NodeStatus::Closed => "closed",
        NodeStatus::Compacted => "compacted",
    }
}

fn outcome_name(outcome: SpawnOutcome) -> &'static str {
    match outcome {
        SpawnOutcome::Completed => "completed",
        SpawnOutcome::Errored => "errored",
        SpawnOutcome::Aborted => "aborted",
    }
}

fn escape_xml_attribute(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
#[path = "spine_context_tests.rs"]
mod tests;
