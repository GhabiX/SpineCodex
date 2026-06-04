use super::SPINE_VIEW_INSTRUCTIONS;
use super::append_spine_view_instructions;

fn occurrences(haystack: &str, needle: &str) -> usize {
    haystack.match_indices(needle).count()
}

#[test]
fn feature_off_leaves_base_instructions_byte_identical() {
    let base = "base instructions\nwith punctuation: !?".to_string();
    let codex_home = tempfile::tempdir().expect("tempdir");
    let actual = append_spine_view_instructions(base.clone(), false, codex_home.path());

    assert_eq!(actual.as_bytes(), base.as_bytes());
}

#[test]
fn feature_on_appends_spine_view_instructions_once() {
    let base = "base instructions".to_string();
    let codex_home = tempfile::tempdir().expect("tempdir");
    let actual = append_spine_view_instructions(base.clone(), true, codex_home.path());

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
    let codex_home = tempfile::tempdir().expect("tempdir");
    let actual = append_spine_view_instructions(base.clone(), true, codex_home.path());

    assert_eq!(actual, base);
    assert_eq!(occurrences(&actual, SPINE_VIEW_INSTRUCTIONS), 1);
}

#[test]
fn feature_on_uses_inst_md_override_from_codex_home() {
    let codex_home = tempfile::tempdir().expect("tempdir");
    let override_instructions = "<spine_view>\nlocal override\n</spine_view>";
    std::fs::write(codex_home.path().join("inst.md"), override_instructions)
        .expect("write override");

    let actual =
        append_spine_view_instructions("base instructions".to_string(), true, codex_home.path());

    assert_eq!(
        actual,
        format!("base instructions\n\n{override_instructions}")
    );
    assert!(!actual.contains(SPINE_VIEW_INSTRUCTIONS));
}

#[test]
fn feature_on_replaces_existing_spine_view_with_inst_md_override() {
    let codex_home = tempfile::tempdir().expect("tempdir");
    let override_instructions = "<spine_view>\nlocal override\n</spine_view>";
    std::fs::write(codex_home.path().join("inst.md"), override_instructions)
        .expect("write override");

    let base = format!("base instructions\n\n{SPINE_VIEW_INSTRUCTIONS}");
    let actual = append_spine_view_instructions(base, true, codex_home.path());

    assert_eq!(
        actual,
        format!("base instructions\n\n{override_instructions}")
    );
    assert!(!actual.contains(SPINE_VIEW_INSTRUCTIONS));
}
