use super::SPINE_VIEW_INSTRUCTIONS;
use super::append_spine_view_instructions;

fn occurrences(haystack: &str, needle: &str) -> usize {
    haystack.match_indices(needle).count()
}

#[test]
fn feature_off_leaves_base_instructions_byte_identical() {
    let base = "base instructions\nwith punctuation: !?".to_string();
    let actual = append_spine_view_instructions(base.clone(), false);

    assert_eq!(actual.as_bytes(), base.as_bytes());
}

#[test]
fn feature_on_appends_spine_view_instructions_once() {
    let base = "base instructions".to_string();
    let actual = append_spine_view_instructions(base.clone(), true);

    assert!(actual.starts_with(&base));
    assert!(actual.len() > base.len());
    assert_eq!(occurrences(&actual, SPINE_VIEW_INSTRUCTIONS), 1);
    assert_eq!(
        actual
            .strip_prefix(&base)
            .expect("base instructions prefix"),
        format!("\n\n{SPINE_VIEW_INSTRUCTIONS}")
    );
}

#[test]
fn feature_on_does_not_append_spine_view_twice() {
    let base = format!("base instructions\n\n{SPINE_VIEW_INSTRUCTIONS}");
    let actual = append_spine_view_instructions(base.clone(), true);

    assert_eq!(actual, base);
    assert_eq!(occurrences(&actual, SPINE_VIEW_INSTRUCTIONS), 1);
}
