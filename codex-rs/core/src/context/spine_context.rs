use super::ContextualUserFragment;
use super::multi_agent_mode_instructions::MultiAgentModeInstructions;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::MULTI_AGENT_MODE_CLOSE_TAG;
use codex_protocol::protocol::MULTI_AGENT_MODE_OPEN_TAG;
use spine_core::NodeContextCost;
use spine_core::NodeId;
use spine_core::NodeStatus;
use spine_core::SpawnOutcome;
use spine_core::SpawnTask;

pub(crate) const MAX_SPINE_FRAGMENT_BYTES: usize = 9_999;

pub(crate) enum SpineMultiAgentModeInstructions {
    Native(MultiAgentModeInstructions),
    Configured(String),
}

impl SpineMultiAgentModeInstructions {
    pub(crate) fn from_mode(mode: codex_protocol::config_types::MultiAgentMode) -> Option<Self> {
        MultiAgentModeInstructions::from_mode(mode).map(Self::Native)
    }
}

impl ContextualUserFragment for SpineMultiAgentModeInstructions {
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
        match self {
            Self::Native(instructions) => instructions.body(),
            Self::Configured(body) => body.clone(),
        }
    }
}

macro_rules! impl_fragment {
    ($name:ident, $role:literal, $start:literal, $end:literal) => {
        impl ContextualUserFragment for $name {
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
        context_cost: NodeContextCost,
        prompt: &str,
    ) -> Result<Self, String> {
        let attributes = format!(
            " id=\"{node_id}\" summary=\"{}\" status=\"{}\">",
            escape_xml_attribute(summary),
            status_name(status),
        );
        let body = if matches!(status, NodeStatus::Live | NodeStatus::Opened) {
            let remaining_context_windows = match context_cost {
                NodeContextCost::Percentage(percent) => {
                    format!("{}%", 100_u64.saturating_sub(percent))
                }
                NodeContextCost::Unavailable => "unavailable".to_string(),
            };
            let prompt = prompt.trim();
            if prompt.is_empty() {
                format!(
                    "{attributes}\nCurrent Remaining Context Windows: \
                     {remaining_context_windows}\n"
                )
            } else {
                format!(
                    "{attributes}\nCurrent Remaining Context Windows: \
                     {remaining_context_windows}\n{prompt}\n"
                )
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

pub(crate) struct SpineSpawnEvidenceFragment {
    body: String,
}

impl SpineSpawnEvidenceFragment {
    pub(crate) fn new(
        owner_node: &NodeId,
        task: &SpawnTask,
        outcome: SpawnOutcome,
        diagnostic: Option<&str>,
        execution_ref: Option<&str>,
    ) -> Result<Self, String> {
        let evidence = serde_json::to_string_pretty(&serde_json::json!({
            "summary": task.summary,
            "prompt": task.prompt,
            "outcome": outcome,
            "diagnostic": diagnostic,
            "execution_ref": execution_ref,
        }))
        .map_err(|error| format!("failed to render Spine spawn evidence: {error}"))?;
        let body = format!(" node_id=\"{owner_node}\">\n{evidence}\n");
        checked_fragment(
            "spawn evidence",
            "<spine_spawn_evidence",
            body,
            "</spine_spawn_evidence>",
        )
        .map(|body| Self { body })
    }
}

impl_fragment!(
    SpineSpawnEvidenceFragment,
    "user",
    "<spine_spawn_evidence",
    "</spine_spawn_evidence>"
);

pub(crate) struct SpineUserAnchor(u64);

impl SpineUserAnchor {
    pub(crate) fn new(anchor: u64) -> Self {
        Self(anchor)
    }

    pub(crate) fn apply(self, item: &mut ResponseItem) {
        let ResponseItem::Message { role, content, .. } = item else {
            return;
        };
        if role != "user" {
            return;
        }
        let prefix = format!("[U{}]\n", self.0);
        if let Some(ContentItem::InputText { text }) = content
            .iter_mut()
            .find(|item| matches!(item, ContentItem::InputText { .. }))
        {
            text.insert_str(0, &prefix);
        } else {
            content.insert(0, ContentItem::InputText { text: prefix });
        }
    }
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
