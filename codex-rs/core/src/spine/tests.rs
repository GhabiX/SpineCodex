use super::*;
use crate::spine::CHECKPOINT_VERSION;
use crate::spine::SpineCloneBoundary;
use crate::spine::archive::memory_ref;
use crate::spine::archive::tree_meta;
use crate::spine::checkpoint::CheckpointMemoryRef;
use crate::spine::compact_checkpoint::CompactCheckpointMemoryItemRef;
use crate::spine::compact_checkpoint::SpineCompactCheckpoint;
use crate::spine::io::hash_response_items;
use crate::spine::io::sha1_hex;
use crate::spine::model::MemKind;
use crate::spine::model::MemRecord;
use crate::spine::model::NodeId;
use crate::spine::model::PressureEvent;
use crate::spine::model::SpineCommitKindMarker;
use crate::spine::model::SpineToken;
use crate::spine::model::ToolCallEventSegment;
use crate::spine::model::ToolCallSegment;
use crate::spine::model::TrimResponseKind;
use crate::spine::render::memory_response_item;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ImageDetail;
use codex_protocol::spine_tree::SpineNodeContextBaselineSource;
use codex_protocol::spine_tree::SpineTreeNodeAccountingSnapshot;
use codex_protocol::spine_tree::SpineTreeNodeStatus;
use serial_test::serial;
use std::path::PathBuf;

#[path = "tests/checkpoint_failures.rs"]
mod checkpoint_failures;
#[path = "tests/checkpoint_failures_hash.rs"]
mod checkpoint_failures_hash;
#[path = "tests/checkpoint_failures_rollback.rs"]
mod checkpoint_failures_rollback;
#[path = "tests/clone_boundary_pressure.rs"]
mod clone_boundary_pressure;
#[path = "tests/clone_boundary_pressure_alias.rs"]
mod clone_boundary_pressure_alias;
#[path = "tests/clone_missing_memory.rs"]
mod clone_missing_memory;
#[path = "tests/clone_missing_memory_alias.rs"]
mod clone_missing_memory_alias;
#[path = "tests/clone_structural_pressure.rs"]
mod clone_structural_pressure;
#[path = "tests/clone_structural_pressure_refs.rs"]
mod clone_structural_pressure_refs;
#[path = "tests/close_commit_artifact_failures.rs"]
mod close_commit_artifact_failures;
#[path = "tests/close_commit_durable_failures.rs"]
mod close_commit_durable_failures;
#[path = "tests/close_commit_failures.rs"]
mod close_commit_failures;
#[path = "tests/close_commit_internal_failures.rs"]
mod close_commit_internal_failures;
#[path = "tests/close_lifecycle.rs"]
mod close_lifecycle;
#[path = "tests/close_memory_assembly.rs"]
mod close_memory_assembly;
#[path = "tests/close_output_projection.rs"]
mod close_output_projection;
#[path = "tests/close_reduce_edges.rs"]
mod close_reduce_edges;
#[path = "tests/close_reduce_edges_open_toolcall.rs"]
mod close_reduce_edges_open_toolcall;
#[path = "tests/close_reduce_failures.rs"]
mod close_reduce_failures;
#[path = "tests/close_retry.rs"]
mod close_retry;
#[path = "tests/close_retry_pending_token.rs"]
mod close_retry_pending_token;
#[path = "tests/close_retry_prepared_memory.rs"]
mod close_retry_prepared_memory;
#[path = "tests/close_source_plan.rs"]
mod close_source_plan;
#[path = "tests/close_source_plan_guards.rs"]
mod close_source_plan_guards;
#[path = "tests/close_source_plan_stale_indices.rs"]
mod close_source_plan_stale_indices;
#[path = "tests/closed_memory_accounting.rs"]
mod closed_memory_accounting;
#[path = "tests/closed_memory_accounting_negative_delta.rs"]
mod closed_memory_accounting_negative_delta;
#[path = "tests/closed_memory_accounting_pending.rs"]
mod closed_memory_accounting_pending;
#[path = "tests/closed_memory_accounting_reload.rs"]
mod closed_memory_accounting_reload;
#[path = "tests/commit_marker_carriers.rs"]
mod commit_marker_carriers;
#[path = "tests/commit_marker_clone_carrier.rs"]
mod commit_marker_clone_carrier;
#[path = "tests/commit_marker_next_carriers.rs"]
mod commit_marker_next_carriers;
#[path = "tests/commit_marker_prepare_failures.rs"]
mod commit_marker_prepare_failures;
#[path = "tests/commit_marker_prepare_failures_next.rs"]
mod commit_marker_prepare_failures_next;
#[path = "tests/commit_marker_replay.rs"]
mod commit_marker_replay;
#[path = "tests/commit_marker_replay_classification.rs"]
mod commit_marker_replay_classification;
#[path = "tests/commit_marker_required.rs"]
mod commit_marker_required;
#[path = "tests/commit_marker_required_next.rs"]
mod commit_marker_required_next;
#[path = "tests/commit_marker_resume_failures.rs"]
mod commit_marker_resume_failures;
#[path = "tests/commit_marker_resume_failures_memory.rs"]
mod commit_marker_resume_failures_memory;
#[path = "tests/commit_marker_root_compact.rs"]
mod commit_marker_root_compact;
#[path = "tests/compact_checkpoint_ambiguity.rs"]
mod compact_checkpoint_ambiguity;
#[path = "tests/compact_checkpoint_ambiguity_proofs.rs"]
mod compact_checkpoint_ambiguity_proofs;
#[path = "tests/compact_checkpoint_clone.rs"]
mod compact_checkpoint_clone;
#[path = "tests/compact_checkpoint_clone_boundary.rs"]
mod compact_checkpoint_clone_boundary;
#[path = "tests/compact_checkpoint_proofs.rs"]
mod compact_checkpoint_proofs;
#[path = "tests/compact_checkpoint_validation.rs"]
mod compact_checkpoint_validation;
#[path = "tests/compact_checkpoint_validation_mismatch.rs"]
mod compact_checkpoint_validation_mismatch;
#[path = "tests/compact_checkpoint_validation_missing_marker.rs"]
mod compact_checkpoint_validation_missing_marker;
#[path = "tests/context_index_regression.rs"]
mod context_index_regression;
#[path = "tests/error_classification.rs"]
mod error_classification;
#[path = "tests/error_classification_runtime.rs"]
mod error_classification_runtime;
#[path = "tests/fork_clone_context_index.rs"]
mod fork_clone_context_index;
#[path = "tests/fork_isolation.rs"]
mod fork_isolation;
#[path = "tests/fork_isolation_aliases.rs"]
mod fork_isolation_aliases;
#[path = "tests/m0_trace.rs"]
mod m0_trace;
#[path = "tests/m0_trace_grouped_close.rs"]
mod m0_trace_grouped_close;
#[path = "tests/materialize_projection.rs"]
mod materialize_projection;
#[path = "tests/materialize_projection_guards.rs"]
mod materialize_projection_guards;
#[path = "tests/materialize_projection_memory.rs"]
mod materialize_projection_memory;
#[path = "tests/materialize_projection_visible_msg_guard.rs"]
mod materialize_projection_visible_msg_guard;
#[path = "tests/materialize_variable_context_for_test.rs"]
mod materialize_variable_context_for_test;
#[path = "tests/message_anchor_image_only.rs"]
mod message_anchor_image_only;
#[path = "tests/message_anchor_multimodal.rs"]
mod message_anchor_multimodal;
#[path = "tests/message_anchor_non_user.rs"]
mod message_anchor_non_user;
#[path = "tests/message_anchor_validation.rs"]
mod message_anchor_validation;
#[path = "tests/message_anchors.rs"]
mod message_anchors;
#[path = "tests/next_failure.rs"]
mod next_failure;
#[path = "tests/next_lifecycle.rs"]
mod next_lifecycle;
#[path = "tests/next_lifecycle_checkpoint.rs"]
mod next_lifecycle_checkpoint;
#[path = "tests/next_provider_baseline.rs"]
mod next_provider_baseline;
#[path = "tests/next_root_cursor_failures.rs"]
mod next_root_cursor_failures;
#[path = "tests/next_root_cursor_failures_next.rs"]
mod next_root_cursor_failures_next;
#[path = "tests/next_transactions.rs"]
mod next_transactions;
#[path = "tests/observe_index_space.rs"]
mod observe_index_space;
#[path = "tests/open_lifecycle.rs"]
mod open_lifecycle;
#[path = "tests/open_lifecycle_duplicates.rs"]
mod open_lifecycle_duplicates;
#[path = "tests/open_lifecycle_failures.rs"]
mod open_lifecycle_failures;
#[path = "tests/parser_boundary.rs"]
mod parser_boundary;
#[path = "tests/pending_control.rs"]
mod pending_control;
#[path = "tests/pending_control_abort_stale.rs"]
mod pending_control_abort_stale;
#[path = "tests/pending_control_raw_requests.rs"]
mod pending_control_raw_requests;
#[path = "tests/pending_control_raw_requests_open.rs"]
mod pending_control_raw_requests_open;
#[path = "tests/pending_control_receipt_abort.rs"]
mod pending_control_receipt_abort;
#[path = "tests/pending_control_receipt_duplicates.rs"]
mod pending_control_receipt_duplicates;
#[path = "tests/pending_control_receipts.rs"]
mod pending_control_receipts;
#[path = "tests/prepared_commit.rs"]
mod prepared_commit;
#[path = "tests/prepared_commit_side_effect_failures.rs"]
mod prepared_commit_side_effect_failures;
#[path = "tests/provider_baseline.rs"]
mod provider_baseline;
#[path = "tests/provider_baseline_capture.rs"]
mod provider_baseline_capture;
#[path = "tests/provider_baseline_capture_replay.rs"]
mod provider_baseline_capture_replay;
#[path = "tests/provider_baseline_legacy_encoding.rs"]
mod provider_baseline_legacy_encoding;
#[path = "tests/provider_baseline_pressure.rs"]
mod provider_baseline_pressure;
#[path = "tests/provider_legacy_pressure.rs"]
mod provider_legacy_pressure;
#[path = "tests/provider_legacy_pressure_corrupt.rs"]
mod provider_legacy_pressure_corrupt;
#[path = "tests/raw_coverage.rs"]
mod raw_coverage;
#[path = "tests/response_fixtures.rs"]
mod response_fixtures;
pub(crate) use response_fixtures::*;
#[path = "tests/rollback_checkpoint_continuation.rs"]
mod rollback_checkpoint_continuation;
#[path = "tests/rollback_checkpoint_continuation_alias.rs"]
mod rollback_checkpoint_continuation_alias;
#[path = "tests/rollback_checkpoint_fail_closed.rs"]
mod rollback_checkpoint_fail_closed;
#[path = "tests/rollback_checkpoint_fail_closed_rendered.rs"]
mod rollback_checkpoint_fail_closed_rendered;
#[path = "tests/rollback_checkpoint_live_append.rs"]
mod rollback_checkpoint_live_append;
#[path = "tests/rollback_checkpoint_live_append_cache.rs"]
mod rollback_checkpoint_live_append_cache;
#[path = "tests/rollback_checkpoint_provider_baseline.rs"]
mod rollback_checkpoint_provider_baseline;
#[path = "tests/rollback_checkpoint_provider_baseline_alias.rs"]
mod rollback_checkpoint_provider_baseline_alias;
#[path = "tests/rollback_checkpoint_records.rs"]
mod rollback_checkpoint_records;
#[path = "tests/rollback_checkpoint_records_initial.rs"]
mod rollback_checkpoint_records_initial;
#[path = "tests/rollback_checkpoint_records_provider.rs"]
mod rollback_checkpoint_records_provider;
#[path = "tests/rollback_checkpoint_restore.rs"]
mod rollback_checkpoint_restore;
#[path = "tests/rollback_checkpoint_restore_alias.rs"]
mod rollback_checkpoint_restore_alias;
#[path = "tests/rollback_sparse.rs"]
mod rollback_sparse;
#[path = "tests/rollback_sparse_hole.rs"]
mod rollback_sparse_hole;
#[path = "tests/rollback_sparse_materialization.rs"]
mod rollback_sparse_materialization;
#[path = "tests/rollback_sparse_stale.rs"]
mod rollback_sparse_stale;
#[path = "tests/root_compact_boundary.rs"]
mod root_compact_boundary;
#[path = "tests/root_compact_boundary_checkpoint.rs"]
mod root_compact_boundary_checkpoint;
#[path = "tests/root_compact_boundary_source_range.rs"]
mod root_compact_boundary_source_range;
#[path = "tests/root_compact_checkpoint_retry.rs"]
mod root_compact_checkpoint_retry;
#[path = "tests/root_compact_example_trace.rs"]
mod root_compact_example_trace;
#[path = "tests/root_compact_failures.rs"]
mod root_compact_failures;
#[path = "tests/root_compact_lifecycle.rs"]
mod root_compact_lifecycle;
#[path = "tests/root_compact_lifecycle_close_open.rs"]
mod root_compact_lifecycle_close_open;
#[path = "tests/root_compact_prepared.rs"]
mod root_compact_prepared;
#[path = "tests/root_compact_prepared_install.rs"]
mod root_compact_prepared_install;
#[path = "tests/root_compact_replay.rs"]
mod root_compact_replay;
#[path = "tests/root_compact_replay_rollback.rs"]
mod root_compact_replay_rollback;
#[path = "tests/root_compact_staging_failures.rs"]
mod root_compact_staging_failures;
#[path = "tests/root_compact_token_baseline.rs"]
mod root_compact_token_baseline;
#[path = "tests/root_compact_token_baseline_handoff.rs"]
mod root_compact_token_baseline_handoff;
#[path = "tests/runtime_lifecycle.rs"]
mod runtime_lifecycle;
#[path = "tests/runtime_lifecycle_trim_ledger.rs"]
mod runtime_lifecycle_trim_ledger;
#[path = "tests/runtime_lifecycle_writer_ownership.rs"]
mod runtime_lifecycle_writer_ownership;
#[path = "tests/runtime_store_fixtures.rs"]
mod runtime_store_fixtures;
pub(crate) use runtime_store_fixtures::*;
#[path = "tests/store_basics.rs"]
mod store_basics;
#[path = "tests/tool_use_failure_ordinary.rs"]
mod tool_use_failure_ordinary;
#[path = "tests/toolcall_grouping.rs"]
mod toolcall_grouping;
#[path = "tests/toolcall_grouping_multi_request.rs"]
mod toolcall_grouping_multi_request;
#[path = "tests/toolcall_grouping_validation.rs"]
mod toolcall_grouping_validation;
#[path = "tests/toolcall_lexer.rs"]
mod toolcall_lexer;
#[path = "tests/toolcall_lexer_replay.rs"]
mod toolcall_lexer_replay;
#[path = "tests/toolcall_pending_response_flush.rs"]
mod toolcall_pending_response_flush;
#[path = "tests/toolcall_spine_tree.rs"]
mod toolcall_spine_tree;
#[path = "tests/tree_accounting.rs"]
mod tree_accounting;
#[path = "tests/tree_snapshot.rs"]
mod tree_snapshot;
#[path = "tests/tree_snapshot_closed_child.rs"]
mod tree_snapshot_closed_child;
#[path = "tests/tree_snapshot_historical_descendants.rs"]
mod tree_snapshot_historical_descendants;
#[path = "tests/tree_snapshot_nested_projection.rs"]
mod tree_snapshot_nested_projection;
#[path = "tests/tree_snapshot_root_compact.rs"]
mod tree_snapshot_root_compact;
#[path = "tests/trim_candidate_rejections.rs"]
mod trim_candidate_rejections;
#[path = "tests/trim_candidate_rejections_short.rs"]
mod trim_candidate_rejections_short;
#[path = "tests/trim_candidates.rs"]
mod trim_candidates;
#[path = "tests/trim_candidates_custom.rs"]
mod trim_candidates_custom;
#[path = "tests/trim_only.rs"]
mod trim_only;
#[path = "tests/trim_only_fork_clone.rs"]
mod trim_only_fork_clone;
#[path = "tests/trim_projection.rs"]
mod trim_projection;
#[path = "tests/trim_projection_composition.rs"]
mod trim_projection_composition;
#[path = "tests/trim_projection_composition_snip.rs"]
mod trim_projection_composition_snip;
#[path = "tests/trim_projection_slice.rs"]
mod trim_projection_slice;
#[path = "tests/trim_projection_slice_anchor.rs"]
mod trim_projection_slice_anchor;
#[path = "tests/trim_projection_slice_rejections.rs"]
mod trim_projection_slice_rejections;
#[path = "tests/trim_projection_slice_tail.rs"]
mod trim_projection_slice_tail;
#[path = "tests/trim_rollback_fork.rs"]
mod trim_rollback_fork;
#[path = "tests/trim_rollback_fork_candidate.rs"]
mod trim_rollback_fork_candidate;
#[path = "tests/trim_rollback_fork_clone.rs"]
mod trim_rollback_fork_clone;
#[path = "tests/trim_targeting.rs"]
mod trim_targeting;
#[path = "tests/trim_targeting_ledger.rs"]
mod trim_targeting_ledger;
#[path = "tests/trim_targeting_no_retry.rs"]
mod trim_targeting_no_retry;
#[path = "tests/workflow_fixtures.rs"]
mod workflow_fixtures;
pub(crate) use workflow_fixtures::*;
