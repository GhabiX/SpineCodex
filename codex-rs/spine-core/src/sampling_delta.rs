use crate::CharParseError;
use crate::ExecutedSpineFact;
use crate::ExecutionId;
use crate::RawBoundary;
use crate::RawSpan;
use crate::RolloutEvent;
use crate::SourceSnapshot;
use crate::SpineCharParser;
use crate::SpineCompactBarrierV1;
use crate::SpineCompiler;
use crate::archive::FactSourceBinding;
use crate::compiler::SamplingCompileError;

#[derive(Debug)]
pub(crate) enum SamplingDeltaError {
    Parse(CharParseError),
    Compile(SamplingCompileError),
    MissingSourceBoundary(RawBoundary),
    FactHasNoSourceGroup(ExecutionId),
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
    }
    for event in parser
        .finish_sampling(post_boundary)
        .map_err(SamplingDeltaError::Parse)?
    {
        retained_bytes = retained_bytes.saturating_add(event.retained_bytes());
    }

    // Native tool items are opaque. Every explicit Spine fact is bound to the
    // complete source block observed during this sampling.
    if let Some(first) = sampling_source.first() {
        let start = first.id.clone();
        let end = snapshot
            .source_at_raw_boundary(post_boundary)
            .ok_or(SamplingDeltaError::MissingSourceBoundary(post_boundary))?
            .id
            .clone();
        for (index, fact) in facts.iter().enumerate() {
            if applied[index] {
                continue;
            }
            if let Some(expected) = expected
                && (expected[index].start != start || expected[index].end != end)
            {
                return Err(SamplingDeltaError::FactSourceExecutionMismatch);
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
        compiler
            .eat_sampling(span, retained_bytes, &fact_refs, open_input_tokens)
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
