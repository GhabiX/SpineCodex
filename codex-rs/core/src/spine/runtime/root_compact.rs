use codex_protocol::models::ContentItem;
use codex_protocol::models::FunctionCallOutputPayload;
use codex_protocol::models::ReasoningItemContent;
use codex_protocol::models::ReasoningItemReasoningSummary;
use codex_protocol::models::ResponseItem;
use std::path::Path;

use super::SpineError;
use super::SpinePreparedRootCompact;
use super::SpinePreparedRootCompactInstall;
use super::SpineRootCompactResult;
use super::SpineRootCompactTokenMetadata;
use super::SpineRuntime;
use super::session_state::PreparedSpineRootCompactCommit;
use crate::spine::archive::memory_ref;
use crate::spine::compact_checkpoint::build_compact_checkpoint;
use crate::spine::io::hash_raw_live;
use crate::spine::io::sha1_hex;
use crate::spine::model::MemKind;
use crate::spine::model::MemRecord;
use crate::spine::model::SpineLedgerEvent;
use crate::spine::model::SpineToken;
use crate::spine::parse_stack::ParseStack;
use crate::spine::parse_stack::PreparedRootEpochReduction;
use crate::spine::render::render_parse_stack_to_context_with_memory_body_and_trim_projection;
use crate::spine::store::BODY_DIR;

struct PreparedRootCompactCommit {
    result: SpineRootCompactResult,
    mem: MemRecord,
    memory_body: String,
    compact_checkpoint: Option<crate::spine::compact_checkpoint::SpineCompactCheckpoint>,
    root_compact_event: SpineLedgerEvent,
    memory: crate::spine::model::MemoryRef,
    root_epoch_reduction: PreparedRootEpochReduction,
    next_open_index: usize,
}

pub(crate) fn spine_root_compact_body(replaced_context: &[ResponseItem]) -> Option<String> {
    let entries = replaced_context
        .iter()
        .enumerate()
        .filter_map(|(index, item)| response_item_visible_text(item).map(|text| (index + 1, text)))
        .collect::<Vec<_>>();
    if entries.is_empty() {
        return None;
    }

    let mut body = "# Spine Native Compact Memory\n\n\
This memory is derived from the host context after native compact succeeded.\n"
        .to_string();
    for (index, text) in entries {
        body.push_str("\n## Replaced Context Item ");
        body.push_str(&index.to_string());
        body.push_str("\n\n");
        body.push_str(text.trim());
        body.push('\n');
    }
    Some(body)
}

fn response_item_visible_text(item: &ResponseItem) -> Option<String> {
    match item {
        ResponseItem::Message { role, content, .. } => {
            let text = content_items_visible_text(content)?;
            Some(format!("{role}: {text}"))
        }
        ResponseItem::Reasoning {
            summary, content, ..
        } => reasoning_visible_text(summary, content.as_deref()),
        ResponseItem::LocalShellCall {
            call_id,
            status,
            action,
            ..
        } => {
            let call_id = call_id.as_deref().unwrap_or("<missing>");
            Some(format!(
                "local_shell_call {call_id} status={status:?}\n{action:?}"
            ))
        }
        ResponseItem::FunctionCall {
            name,
            namespace,
            arguments,
            call_id,
            ..
        } => {
            let tool_name = namespace
                .as_deref()
                .map(|namespace| format!("{namespace}.{name}"))
                .unwrap_or_else(|| name.clone());
            if arguments.trim().is_empty() {
                Some(format!("function_call {call_id}: {tool_name}"))
            } else {
                Some(format!(
                    "function_call {call_id}: {tool_name}\narguments: {arguments}"
                ))
            }
        }
        ResponseItem::ToolSearchCall {
            call_id,
            status,
            execution,
            arguments,
            ..
        } => {
            let call_id = call_id.as_deref().unwrap_or("<missing>");
            let status = status.as_deref().unwrap_or("<unknown>");
            Some(format!(
                "tool_search_call {call_id} status={status} execution={execution}\narguments: {arguments}"
            ))
        }
        ResponseItem::FunctionCallOutput { call_id, output } => {
            function_call_output_visible_text(output)
                .map(|text| format!("function_call_output {call_id}: {text}"))
        }
        ResponseItem::CustomToolCall {
            call_id,
            name,
            input,
            status,
            ..
        } => {
            let status = status.as_deref().unwrap_or("<unknown>");
            if input.trim().is_empty() {
                Some(format!(
                    "custom_tool_call {call_id}: {name} status={status}"
                ))
            } else {
                Some(format!(
                    "custom_tool_call {call_id}: {name} status={status}\ninput: {input}"
                ))
            }
        }
        ResponseItem::CustomToolCallOutput {
            call_id,
            name,
            output,
        } => function_call_output_visible_text(output).map(|text| {
            let name = name.as_deref().unwrap_or("<unknown>");
            format!("custom_tool_call_output {call_id}: {name}: {text}")
        }),
        ResponseItem::ToolSearchOutput {
            call_id,
            status,
            execution,
            tools,
        } => {
            let call_id = call_id.as_deref().unwrap_or("<missing>");
            let tools_text = serde_json::to_string(tools).unwrap_or_else(|_| "[]".to_string());
            Some(format!(
                "tool_search_output {call_id} status={status} execution={execution}\ntools: {tools_text}"
            ))
        }
        ResponseItem::WebSearchCall { status, action, .. } => {
            let status = status.as_deref().unwrap_or("<unknown>");
            Some(format!(
                "web_search_call status={status}\naction: {action:?}"
            ))
        }
        ResponseItem::ImageGenerationCall {
            status,
            revised_prompt,
            ..
        } => {
            let prompt = revised_prompt
                .as_deref()
                .filter(|prompt| !prompt.trim().is_empty())
                .unwrap_or("<none>");
            Some(format!(
                "image_generation_call status={status}\nrevised_prompt: {prompt}"
            ))
        }
        ResponseItem::Compaction { encrypted_content } => {
            non_empty_text(encrypted_content).map(|text| format!("compaction: {text}"))
        }
        ResponseItem::ContextCompaction {
            encrypted_content: Some(encrypted_content),
        } => non_empty_text(encrypted_content).map(|text| format!("context_compaction: {text}")),
        ResponseItem::ContextCompaction {
            encrypted_content: None,
        }
        | ResponseItem::CompactionTrigger
        | ResponseItem::Other => None,
    }
}

fn content_items_visible_text(content: &[ContentItem]) -> Option<String> {
    let text = content
        .iter()
        .filter_map(|item| match item {
            ContentItem::InputText { text } | ContentItem::OutputText { text } => {
                non_empty_text(text)
            }
            ContentItem::InputImage { .. } => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    non_empty_text(&text).map(str::to_string)
}

fn reasoning_visible_text(
    summary: &[ReasoningItemReasoningSummary],
    content: Option<&[ReasoningItemContent]>,
) -> Option<String> {
    let mut parts = Vec::new();
    for item in summary {
        let ReasoningItemReasoningSummary::SummaryText { text } = item;
        if let Some(text) = non_empty_text(text) {
            parts.push(format!("reasoning_summary: {text}"));
        }
    }
    if let Some(content) = content {
        for item in content {
            match item {
                ReasoningItemContent::ReasoningText { text }
                | ReasoningItemContent::Text { text } => {
                    if let Some(text) = non_empty_text(text) {
                        parts.push(format!("reasoning_content: {text}"));
                    }
                }
            }
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n"))
    }
}

fn function_call_output_visible_text(output: &FunctionCallOutputPayload) -> Option<String> {
    output
        .body
        .to_text()
        .and_then(|text| non_empty_text(&text).map(str::to_string))
}

fn non_empty_text(text: &str) -> Option<&str> {
    let text = text.trim();
    (!text.is_empty()).then_some(text)
}

impl SpineRuntime {
    #[cfg(test)]
    pub(crate) fn root_compact(
        &mut self,
        body: String,
        raw_items: &[Option<ResponseItem>],
    ) -> Result<Vec<ResponseItem>, SpineError> {
        let prepared = self.root_compact_impl(
            body,
            raw_items,
            SpineRootCompactTokenMetadata::default(),
            None,
        )?;
        let result = prepared.result.clone();
        self.install_prepared_root_compact(prepared);
        Ok(result.materialized)
    }

    pub(crate) fn root_compact_with_checkpoint(
        &mut self,
        rollout_path: &Path,
        body: String,
        raw_items: &[Option<ResponseItem>],
        token_metadata: SpineRootCompactTokenMetadata,
    ) -> Result<SpineRootCompactResult, SpineError> {
        let prepared = self.prepare_root_compact_with_checkpoint(
            rollout_path,
            body,
            raw_items,
            token_metadata,
        )?;
        let result = prepared.result.clone();
        self.install_prepared_root_compact(prepared);
        Ok(result)
    }

    pub(crate) fn prepare_root_compact_with_checkpoint(
        &mut self,
        rollout_path: &Path,
        body: String,
        raw_items: &[Option<ResponseItem>],
        token_metadata: SpineRootCompactTokenMetadata,
    ) -> Result<SpinePreparedRootCompact, SpineError> {
        self.root_compact_impl(body, raw_items, token_metadata, Some(rollout_path))
    }

    pub(crate) fn prepare_root_compact_install_with_checkpoint(
        &mut self,
        rollout_path: &Path,
        body: String,
        raw_items: &[Option<ResponseItem>],
        token_metadata: SpineRootCompactTokenMetadata,
    ) -> Result<SpinePreparedRootCompactInstall, SpineError> {
        self.prepare_root_compact_with_checkpoint(rollout_path, body, raw_items, token_metadata)
            .map(SpinePreparedRootCompact::into_install)
    }

    pub(crate) fn prepare_root_compact_commit_with_checkpoint(
        &mut self,
        rollout_path: &Path,
        body: String,
        raw_items: &[Option<ResponseItem>],
        token_metadata: SpineRootCompactTokenMetadata,
    ) -> Result<PreparedSpineRootCompactCommit, SpineError> {
        self.prepare_root_compact_install_with_checkpoint(
            rollout_path,
            body,
            raw_items,
            token_metadata,
        )
        .map(PreparedSpineRootCompactCommit::from_install)
    }

    fn root_compact_impl(
        &mut self,
        body: String,
        raw_items: &[Option<ResponseItem>],
        token_metadata: SpineRootCompactTokenMetadata,
        checkpoint_rollout_path: Option<&Path>,
    ) -> Result<SpinePreparedRootCompact, SpineError> {
        let token_metadata = SpineRootCompactTokenMetadata {
            next_open_input_tokens: None,
            next_open_context_tokens: None,
            ..token_metadata
        };
        let prepared = self.prepare_root_compact_commit(
            body,
            raw_items,
            token_metadata,
            checkpoint_rollout_path,
        )?;
        let mut pending_compact_parse_stack = self.parse_stack.clone();
        pending_compact_parse_stack.shift_pending_compact(
            prepared.memory.clone(),
            prepared.next_open_index,
            token_metadata.next_open_input_tokens,
            token_metadata.next_open_context_tokens,
            &self.archive(),
        )?;
        let final_parse_stack = self.root_epoch_reduced_from(
            pending_compact_parse_stack.clone(),
            prepared.root_epoch_reduction,
        )?;
        if let Err(err) = self.commit_root_compact_prepared_side_effects(
            &prepared.mem,
            &prepared.memory_body,
            prepared.compact_checkpoint.as_ref(),
        ) {
            self.parse_stack = pending_compact_parse_stack;
            return Err(err);
        }
        let marker =
            super::support::root_compact_commit_marker(self.ledger.next_event_seq, &prepared.mem)?;
        self.append_committed_events(vec![prepared.root_compact_event], marker)?;
        self.pending = None;
        Ok(SpinePreparedRootCompact {
            result: prepared.result,
            final_parse_stack,
        })
    }

    fn root_epoch_reduced_from(
        &self,
        parse_stack: ParseStack,
        reduction: PreparedRootEpochReduction,
    ) -> Result<ParseStack, SpineError> {
        parse_stack.root_epoch_reduced(reduction)
    }

    pub(crate) fn install_prepared_root_compact(&mut self, prepared: SpinePreparedRootCompact) {
        self.parse_stack = prepared.final_parse_stack;
    }

    pub(crate) fn install_prepared_root_compact_install(
        &mut self,
        install: SpinePreparedRootCompactInstall,
    ) {
        self.install_prepared_root_compact(install.into_prepared());
    }

    fn commit_root_compact_prepared_side_effects(
        &mut self,
        mem: &MemRecord,
        memory_body: &str,
        compact_checkpoint: Option<&crate::spine::compact_checkpoint::SpineCompactCheckpoint>,
    ) -> Result<(), SpineError> {
        self.write_prepared_memory_body(mem, memory_body)
            .and_then(|()| self.commit_prepared_memory_record(mem, memory_body))
            .and_then(|()| {
                if let Some(checkpoint) = compact_checkpoint {
                    self.store.append_compact_checkpoint(checkpoint)?;
                }
                Ok(())
            })
    }

    fn prepare_root_compact_commit(
        &self,
        body: String,
        raw_items: &[Option<ResponseItem>],
        token_metadata: SpineRootCompactTokenMetadata,
        checkpoint_rollout_path: Option<&Path>,
    ) -> Result<PreparedRootCompactCommit, SpineError> {
        if body.trim().is_empty() {
            return Err(SpineError::CompactFailure(
                "spine root compact memory body must not be empty".to_string(),
            ));
        }
        let source_context_end = self.materialize_history(raw_items)?.len();
        let node = self.parse_stack.current_root_epoch_id()?;
        let compact_id = format!("root-{}-{}", node.as_path().replace('.', "-"), self.raw_len);
        let raw_live_hash = hash_raw_live(&self.raw_live);
        let body_hash = sha1_hex(body.as_bytes());
        let mem = MemRecord {
            compact_id: compact_id.clone(),
            kind: MemKind::RootEpoch,
            node: node.clone(),
            raw_start: 0,
            raw_end: self.raw_len,
            context_start: 0,
            context_end: source_context_end,
            raw_live_hash: Some(raw_live_hash.clone()),
            open_input_tokens: None,
            close_input_tokens: token_metadata.close_input_tokens,
            open_context_tokens: None,
            close_context_tokens: token_metadata.close_context_tokens,
            closed_source_suffix_tokens: None,
            closed_memory_context_tokens: None,
            open_context_source: None,
            memory_output_tokens: None,
            body_path: format!("{BODY_DIR}/{compact_id}.md"),
            body_hash,
        };
        let seq = self.ledger.next_event_seq;
        let memory = memory_ref(
            &self.archive(),
            mem.compact_id.clone(),
            mem.node.clone(),
            mem.body_hash.clone(),
            mem.raw_start..mem.raw_end,
            mem.context_start..mem.context_end,
            seq..seq + 1,
            mem.open_input_tokens,
            mem.close_input_tokens,
            mem.open_context_tokens,
            mem.close_context_tokens,
            mem.closed_source_suffix_tokens,
            mem.closed_memory_context_tokens,
            mem.open_context_source,
            mem.memory_output_tokens,
        );

        let staged_memory_body = Some((compact_id.as_str(), body.as_str()));
        let trim_projection = self.current_trim_projection()?;
        let next_open_index_usize = match self.parse_stack.pending_compact_next_open_index(
            &memory,
            token_metadata.next_open_input_tokens,
            token_metadata.next_open_context_tokens,
        )? {
            Some(next_open_index) => next_open_index,
            None => {
                // Probe first because source_context_range records the pre-compact source
                // span, while next_open_index is the post-compact h(PS) materialized len.
                let mut probe_parse_stack = self.parse_stack.clone();
                probe_parse_stack.shift(
                    SpineToken::Compact {
                        memory: memory.clone(),
                        next_open_index: 0,
                        next_open_input_tokens: token_metadata.next_open_input_tokens,
                        next_open_context_tokens: token_metadata.next_open_context_tokens,
                    },
                    &self.archive(),
                )?;
                render_parse_stack_to_context_with_memory_body_and_trim_projection(
                    &probe_parse_stack,
                    raw_items,
                    staged_memory_body,
                    &trim_projection,
                )?
                .len()
            }
        };

        let mut staged_parse_stack = self.parse_stack.clone();
        staged_parse_stack.shift_pending_compact(
            memory.clone(),
            next_open_index_usize,
            token_metadata.next_open_input_tokens,
            token_metadata.next_open_context_tokens,
            &self.archive(),
        )?;
        let root_epoch_reduction = staged_parse_stack.prepare_root_epoch_reduction(
            &self.archive(),
            memory.clone(),
            next_open_index_usize,
            token_metadata.next_open_input_tokens,
            token_metadata.next_open_context_tokens,
        )?;
        staged_parse_stack.apply_prevalidated_root_epoch_reduction(root_epoch_reduction.clone());
        let materialized = render_parse_stack_to_context_with_memory_body_and_trim_projection(
            &staged_parse_stack,
            raw_items,
            staged_memory_body,
            &trim_projection,
        )?;
        let current_open_index = staged_parse_stack.current_open_meta()?.index;
        if current_open_index != materialized.len() {
            return Err(SpineError::Invariant(format!(
                "spine root compact open index {current_open_index} does not match materialized history length {}",
                materialized.len()
            )));
        }
        let next_open_index_u64 = u64::try_from(next_open_index_usize)
            .map_err(|_| SpineError::InvalidEvent("root open index overflow".to_string()))?;
        let token_seq_after = seq.checked_add(1).ok_or_else(|| {
            SpineError::InvalidEvent("root compact token seq overflow".to_string())
        })?;
        let result = SpineRootCompactResult {
            materialized,
            raw_boundary: self.raw_len,
            token_seq_after,
        };
        let compact_checkpoint = checkpoint_rollout_path
            .map(|rollout_path| {
                build_compact_checkpoint(
                    rollout_path,
                    result.raw_boundary,
                    result.token_seq_after,
                    &self.raw_live,
                    raw_items,
                    &staged_parse_stack,
                    &result.materialized,
                    &result.materialized,
                )
            })
            .transpose()?;
        let root_compact_event = SpineLedgerEvent::RootCompact {
            node,
            boundary: self.raw_len,
            mem: compact_id,
            next_open_index: next_open_index_u64,
            raw_live_hash,
            next_open_input_tokens: token_metadata.next_open_input_tokens,
            next_open_context_tokens: token_metadata.next_open_context_tokens,
        };
        Ok(PreparedRootCompactCommit {
            result,
            mem,
            memory_body: body,
            compact_checkpoint,
            root_compact_event,
            memory,
            root_epoch_reduction,
            next_open_index: next_open_index_usize,
        })
    }
}
