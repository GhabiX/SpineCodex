use super::*;

#[test]
fn source_plan_validator_rejects_non_contiguous_context_indices() {
    let raw_items = vec![
        user_message("prefix 0"),
        user_message("prefix 1"),
        assistant_message("source 2"),
        assistant_message("gap 3"),
        assistant_message("source 4"),
    ];
    let plan = source_plan_with_context_range(
        2..5,
        vec![
            source_entry(2, 0, raw_items[2].clone(), false),
            source_entry(4, 1, raw_items[4].clone(), false),
        ],
    );

    let err = validate_source_plan_against_history(&plan, &raw_items, "close")
        .expect_err("non-contiguous real context indices must fail");
    assert!(
        err.to_string()
            .contains("source entry count 2 does not match source context range length 3"),
        "unexpected non-contiguous context error: {err}"
    );
}

#[test]
fn source_plan_validator_rejects_duplicate_context_indices() {
    let raw_items = vec![
        user_message("prefix 0"),
        user_message("prefix 1"),
        assistant_message("source 2"),
        assistant_message("source 3"),
    ];
    let plan = source_plan_with_context_range(
        2..4,
        vec![
            source_entry(2, 0, raw_items[2].clone(), false),
            source_entry(2, 1, raw_items[2].clone(), false),
        ],
    );

    let err = validate_source_plan_against_history(&plan, &raw_items, "close")
        .expect_err("duplicate context indices must fail");
    assert!(
        err.to_string().contains("has context_index 2, expected 3"),
        "unexpected duplicate context error: {err}"
    );
}
