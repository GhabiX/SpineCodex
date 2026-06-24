use codex_protocol::models::ContentItem;
use codex_protocol::models::FunctionCallOutputPayload;
use codex_protocol::models::ReasoningItemContent;
use codex_protocol::models::ReasoningItemReasoningSummary;
use codex_protocol::models::ResponseItem;
use std::path::Path;

use super::SpineError;
use super::SpinePreparedRootCompact;
use super::SpineRootCompactResult;
use super::SpineRootCompactTokenMetadata;
use super::SpineRuntime;
use crate::spine::archive::memory_ref;
use crate::spine::io::hash_raw_live;
use crate::spine::io::sha1_hex;
use crate::spine::model::MemKind;
use crate::spine::model::MemRecord;
use crate::spine::model::SpineLedgerEvent;
use crate::spine::parser::ParserRootCompactInstall;
use crate::spine::parser::ParserRootCompactPendingInstall;
use crate::spine::store::BODY_DIR;

struct PreparedRootCompactCommit {
    result: SpineRootCompactResult,
    mem: MemRecord,
    memory_body: String,
    compact_checkpoint: Option<crate::spine::compact_checkpoint::SpineCompactCheckpoint>,
    root_compact_event: SpineLedgerEvent,
    pending_parser_install: ParserRootCompactPendingInstall,
    parser_install: ParserRootCompactInstall,
}

pub(crate) fn spine_root_compact_body(replaced_context: &[ResponseItem]) -> Option<String> {
    let entries = replaced_context
        .iter()
        .enumerate()
        .filter_map(|(index, item)| response_item_visible_text(item).map(|text| (index + 1, text)))
        .map(|(index, text)| format!("\n## Replaced Context Item {index}\n\n{}\n", text.trim()))
        .collect::<String>();
    (!entries.is_empty()).then(|| {
        format!(
            "# Spine Native Compact Memory\n\n\
This memory is derived from the host context after native compact succeeded.\n{entries}"
        )
    })
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
    (!parts.is_empty()).then(|| parts.join("\n"))
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
        let result = prepared.publication_result().clone();
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
        let result = prepared.publication_result().clone();
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

    pub(crate) fn prepare_root_compact_commit_with_checkpoint(
        &mut self,
        rollout_path: &Path,
        body: String,
        raw_items: &[Option<ResponseItem>],
        token_metadata: SpineRootCompactTokenMetadata,
    ) -> Result<SpinePreparedRootCompact, SpineError> {
        self.prepare_root_compact_with_checkpoint(rollout_path, body, raw_items, token_metadata)
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
        if let Err(err) = self.commit_root_compact_prepared_side_effects(
            &prepared.mem,
            &prepared.memory_body,
            prepared.compact_checkpoint.as_ref(),
        ) {
            self.parser
                .install_pending_root_compact_after_side_effect_failure(
                    prepared.pending_parser_install,
                );
            return Err(err);
        }
        let marker =
            super::support::root_compact_commit_marker(self.ledger.next_event_seq, &prepared.mem)?;
        self.append_committed_events(vec![prepared.root_compact_event], marker)?;
        self.pending = None;
        Ok(SpinePreparedRootCompact::new(
            prepared.result,
            prepared.parser_install,
        ))
    }

    pub(crate) fn install_prepared_root_compact(&mut self, prepared: SpinePreparedRootCompact) {
        self.parser
            .install_prepared_root_compact(prepared.into_parser_install());
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
        let trim_projection = self.current_trim_projection()?;
        let source_context_end = self
            .parser
            .materialized_variable_context_len(raw_items, &trim_projection)?;
        let node = self.parser.current_root_epoch_id()?;
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
        let next_open_index_usize = self.parser.root_compact_next_open_index_or_probe(
            &memory,
            token_metadata.next_open_input_tokens,
            token_metadata.next_open_context_tokens,
            raw_items,
            staged_memory_body,
            &trim_projection,
            &self.archive(),
        )?;

        let prepared_reduction = self.parser.prepare_root_compact_reduction(
            memory.clone(),
            next_open_index_usize,
            token_metadata.next_open_input_tokens,
            token_metadata.next_open_context_tokens,
            raw_items,
            staged_memory_body,
            &trim_projection,
            &self.archive(),
        )?;
        prepared_reduction.validate_current_open_matches_materialized_len()?;
        let token_seq_after = seq.checked_add(1).ok_or_else(|| {
            SpineError::InvalidEvent("root compact token seq overflow".to_string())
        })?;
        let compact_checkpoint = checkpoint_rollout_path
            .map(|rollout_path| {
                prepared_reduction.build_compact_checkpoint(
                    rollout_path,
                    self.raw_len,
                    token_seq_after,
                    &self.raw_live,
                    raw_items,
                )
            })
            .transpose()?;
        let (materialized, pending_parser_install, parser_install) =
            prepared_reduction.into_publication_materialized_and_install();
        let result = SpineRootCompactResult {
            materialized,
            raw_boundary: self.raw_len,
            token_seq_after,
        };
        let (root_compact_event, _token) = crate::spine::lexer::plan_root_compact()
            .lex_event_token(
                node,
                self.raw_len,
                memory.clone(),
                next_open_index_usize,
                raw_live_hash,
                token_metadata.next_open_input_tokens,
                token_metadata.next_open_context_tokens,
            )?;
        Ok(PreparedRootCompactCommit {
            result,
            mem,
            memory_body: body,
            compact_checkpoint,
            root_compact_event,
            pending_parser_install,
            parser_install,
        })
    }
}
