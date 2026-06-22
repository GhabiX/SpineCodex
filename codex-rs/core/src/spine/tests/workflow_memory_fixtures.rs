use super::*;

pub(super) fn memory_assembly_with_context_range(
    node_id: &str,
    source_context_range: Range<usize>,
) -> SpineCloseMemoryAssembly {
    let source_raw_range = u64::try_from(source_context_range.start).expect("range start fits u64")
        ..u64::try_from(source_context_range.end).expect("range end fits u64");
    memory_assembly_with_ranges(node_id, source_context_range, source_raw_range)
}

pub(super) fn memory_assembly_with_ranges(
    node_id: &str,
    source_context_range: Range<usize>,
    source_raw_range: Range<u64>,
) -> SpineCloseMemoryAssembly {
    SpineCloseMemoryAssembly {
        body: format!("# Spine Memory {node_id}\n\nreal compact body for {node_id}\n"),
        source_context_range,
        source_raw_range,
        memory_output_tokens: Some(1_250),
    }
}
