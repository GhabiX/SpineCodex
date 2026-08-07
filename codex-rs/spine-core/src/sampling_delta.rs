use crate::CharParseError;
use crate::ExecutedSpineFact;
use crate::ExecutionId;
use crate::ExecutionOrigin;
use crate::RawBoundary;
use crate::RawSpan;
use crate::RolloutEvent;
use crate::SourceCellId;
use crate::SourceSnapshot;
use crate::SpineCharParser;
use crate::SpineCompactBarrierV1;
use crate::SpineCompiler;
use crate::SpineOperationFact;
use crate::archive::FactSourceBinding;
use crate::compiler::SamplingCompileError;
use crate::context_char::CompletedCalls;

#[derive(Debug)]
pub(crate) enum SamplingDeltaError {
    Parse(CharParseError),
    Compile(SamplingCompileError),
    MissingSourceBoundary(RawBoundary),
    MissingTrimSource(SourceCellId),
    FactHasNoSourceGroup(ExecutionId),
    FactSourceAppliedMoreThanOnce,
    FactSourceExecutionMismatch,
}

pub(crate) enum FactBindingMode<'a> {
    Derive,
    Verify(&'a [FactSourceBinding]),
}

pub(crate) struct SamplingDelta<'a> {
    pub(crate) snapshot: &'a SourceSnapshot,
    pub(crate) committed_source_cells: usize,
    pub(crate) pre_boundary: RawBoundary,
    pub(crate) post_boundary: RawBoundary,
    pub(crate) facts: &'a [ExecutedSpineFact],
    pub(crate) open_input_tokens: Option<u64>,
    pub(crate) binding_mode: FactBindingMode<'a>,
}

/// Projects source observed so far without closing the active sampling boundary.
pub(crate) fn preview_source_delta(
    snapshot: &SourceSnapshot,
    committed_source_cells: usize,
    parser: &mut SpineCharParser,
    compiler: &mut SpineCompiler,
) -> Result<(), SamplingDeltaError> {
    for cell in &snapshot.cells()[committed_source_cells..] {
        let step = parser
            .eat(cell.character())
            .map_err(SamplingDeltaError::Parse)?;
        for event in step.events() {
            compiler
                .eat_source(event.clone())
                .map_err(SamplingDeltaError::Compile)?;
        }
        for completed in step.completed_calls() {
            compiler.observe_completed_calls(completed);
        }
    }
    Ok(())
}

/// Reduces exactly one sampling's source delta through the parser and compiler.
///
/// JIT derives stable fact/source bindings from execution origins. AoT verifies
/// the durable bindings, but both paths execute this same transition kernel.
pub(crate) fn reduce_sampling_delta(
    delta: SamplingDelta<'_>,
    parser: &mut SpineCharParser,
    compiler: &mut SpineCompiler,
) -> Result<Vec<FactSourceBinding>, SamplingDeltaError> {
    let SamplingDelta {
        snapshot,
        committed_source_cells,
        pre_boundary,
        post_boundary,
        facts,
        open_input_tokens,
        binding_mode,
    } = delta;
    let expected = match binding_mode {
        FactBindingMode::Derive => None,
        FactBindingMode::Verify(bindings) => {
            if bindings.len() != facts.len()
                || facts
                    .iter()
                    .zip(bindings)
                    .any(|(fact, binding)| fact.execution_id != binding.execution_id)
            {
                return Err(SamplingDeltaError::FactSourceExecutionMismatch);
            }
            Some(bindings)
        }
    };
    let source_tail = &snapshot.cells()[committed_source_cells..];
    let mut applied = vec![false; facts.len()];
    let mut bindings = vec![None; facts.len()];
    let mut completed_calls = Vec::new();
    let mut retained_bytes = 0usize;

    let sampling_start =
        source_tail.partition_point(|cell| cell.boundary.ordinal() <= pre_boundary.0);
    for cell in &source_tail[..sampling_start] {
        let step = parser
            .eat(cell.character())
            .map_err(SamplingDeltaError::Parse)?;
        for event in step.events() {
            compiler
                .eat_source(event.clone())
                .map_err(SamplingDeltaError::Compile)?;
        }
        for completed in step.completed_calls() {
            compiler.observe_completed_calls(completed);
        }
    }
    for event in parser
        .finish_sampling(pre_boundary)
        .map_err(SamplingDeltaError::Parse)?
    {
        compiler
            .eat_source(event)
            .map_err(SamplingDeltaError::Compile)?;
    }

    let sampling_source = &source_tail[sampling_start..];
    for cell in sampling_source {
        let step = parser
            .eat(cell.character())
            .map_err(SamplingDeltaError::Parse)?;
        for event in step.events() {
            retained_bytes = retained_bytes.saturating_add(event.retained_bytes());
        }
        completed_calls.extend(step.completed_calls().iter().cloned());
    }
    for event in parser
        .finish_sampling(post_boundary)
        .map_err(SamplingDeltaError::Parse)?
    {
        retained_bytes = retained_bytes.saturating_add(event.retained_bytes());
    }

    for completed in &completed_calls {
        let (start, end) = completed_source_span(snapshot, completed)?;
        for (index, fact) in facts.iter().enumerate() {
            let matches = expected.map_or_else(
                || completed_contains_origin(completed, &fact.origin),
                |expected| {
                    expected[index].start == start
                        && expected[index].end == end
                        && completed_contains_origin(completed, &fact.origin)
                },
            );
            if !matches {
                continue;
            }
            if applied[index] {
                return Err(SamplingDeltaError::FactSourceAppliedMoreThanOnce);
            }
            applied[index] = true;
            bindings[index] = Some(FactSourceBinding {
                execution_id: fact.execution_id.clone(),
                start: start.clone(),
                end: end.clone(),
            });
        }
    }

    if let Some(first) = sampling_source.first() {
        let span = RawSpan {
            start: RawBoundary(first.boundary.ordinal()),
            end: post_boundary,
        };
        let fact_refs = facts.iter().collect::<Vec<_>>();
        let trims = resolve_trim_boundaries(snapshot, &fact_refs)?;
        compiler
            .eat_sampling(
                span,
                retained_bytes,
                &completed_calls,
                &fact_refs,
                &trims,
                open_input_tokens,
            )
            .map_err(SamplingDeltaError::Compile)?;
    }

    facts
        .iter()
        .zip(applied)
        .zip(bindings)
        .map(|((fact, applied), binding)| {
            if !applied {
                return Err(SamplingDeltaError::FactHasNoSourceGroup(
                    fact.execution_id.clone(),
                ));
            }
            binding.ok_or(SamplingDeltaError::FactSourceExecutionMismatch)
        })
        .collect()
}

/// Reduces the source tail and closes the current epoch at one compact barrier.
///
/// JIT and AoT keep different source-of-truth inputs, but compact must perform
/// the same parser/compiler transition in both modes.
pub(crate) fn reduce_compact_delta(
    snapshot: &SourceSnapshot,
    committed_source_cells: usize,
    barrier: &SpineCompactBarrierV1,
    parser: &mut SpineCharParser,
    compiler: &mut SpineCompiler,
) -> Result<(), SamplingDeltaError> {
    if committed_source_cells != snapshot.cells().len() {
        let post_boundary = snapshot
            .last_boundary()
            .map(|boundary| RawBoundary(boundary.ordinal()))
            .unwrap_or(barrier.boundary);
        reduce_sampling_delta(
            SamplingDelta {
                snapshot,
                committed_source_cells,
                pre_boundary: post_boundary,
                post_boundary,
                facts: &[],
                open_input_tokens: None,
                binding_mode: FactBindingMode::Derive,
            },
            parser,
            compiler,
        )?;
    }
    for event in parser
        .finish_epoch(barrier.boundary)
        .map_err(SamplingDeltaError::Parse)?
    {
        compiler
            .eat_source(event)
            .map_err(SamplingDeltaError::Compile)?;
    }
    compiler
        .eat_source(RolloutEvent::Compact {
            boundary: barrier.boundary,
            replacement_history: Vec::new(),
        })
        .map_err(SamplingDeltaError::Compile)?;
    *parser = SpineCharParser::default();
    for boundary in &barrier.replacement_boundaries {
        let step = parser
            .eat(crate::SpineChar::Opaque {
                boundary: *boundary,
            })
            .map_err(SamplingDeltaError::Parse)?;
        for event in step.events() {
            compiler
                .eat_source(event.clone())
                .map_err(SamplingDeltaError::Compile)?;
        }
    }
    Ok(())
}

fn completed_contains_origin(completed: &CompletedCalls, origin: &ExecutionOrigin) -> bool {
    let call_id = match origin {
        ExecutionOrigin::Direct { call_id } => call_id,
    };
    completed.calls.iter().any(|call| call.call_id == *call_id)
}

fn completed_source_span(
    snapshot: &SourceSnapshot,
    completed: &CompletedCalls,
) -> Result<(SourceCellId, SourceCellId), SamplingDeltaError> {
    let start = snapshot
        .source_at_raw_boundary(completed.span.start)
        .ok_or(SamplingDeltaError::MissingSourceBoundary(
            completed.span.start,
        ))?
        .id
        .clone();
    let end = snapshot
        .source_at_raw_boundary(completed.span.end)
        .ok_or(SamplingDeltaError::MissingSourceBoundary(
            completed.span.end,
        ))?
        .id
        .clone();
    Ok((start, end))
}

fn resolve_trim_boundaries<'a>(
    source: &SourceSnapshot,
    facts: &[&'a ExecutedSpineFact],
) -> Result<Vec<(RawBoundary, &'a ExecutedSpineFact)>, SamplingDeltaError> {
    facts
        .iter()
        .filter_map(|fact| match &fact.operation {
            SpineOperationFact::Trim { target, .. } => Some(
                source
                    .boundary(&target.response)
                    .map(|boundary| (RawBoundary(boundary.ordinal()), *fact))
                    .ok_or_else(|| SamplingDeltaError::MissingTrimSource(target.response.clone())),
            ),
            SpineOperationFact::Open { .. }
            | SpineOperationFact::Close { .. }
            | SpineOperationFact::Next { .. }
            | SpineOperationFact::Spawn { .. } => None,
        })
        .collect()
}
