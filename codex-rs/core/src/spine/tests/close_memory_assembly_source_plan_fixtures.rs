use super::*;

pub(crate) fn source_plan(
    entries: Vec<crate::spine::SpineCompactSourcePlanEntry>,
) -> SpineCompactSourcePlan {
    SpineCompactSourcePlan {
        node_id: node_id(&[1, 1]),
        source_context_range: 2..2 + entries.len(),
        source_raw_range: 2..2 + u64::try_from(entries.len()).expect("entries len fits u64"),
        entries,
    }
}

pub(crate) fn source_plan_with_context_range(
    source_context_range: std::ops::Range<usize>,
    entries: Vec<crate::spine::SpineCompactSourcePlanEntry>,
) -> SpineCompactSourcePlan {
    SpineCompactSourcePlan {
        node_id: node_id(&[1, 1]),
        source_raw_range: u64::try_from(source_context_range.start).expect("range start fits u64")
            ..u64::try_from(source_context_range.end).expect("range end fits u64"),
        source_context_range,
        entries,
    }
}
