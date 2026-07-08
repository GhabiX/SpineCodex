use super::SPINE_JIT_INSTRUCTIONS;
use super::append_spine_view_instructions;

fn occurrences(haystack: &str, needle: &str) -> usize {
    haystack.match_indices(needle).count()
}

#[test]
fn feature_off_leaves_base_instructions_byte_identical() {
    let base = "base instructions\nwith punctuation: !?".to_string();
    let codex_home = tempfile::tempdir().expect("tempdir");
    let actual =
        append_spine_view_instructions(base.clone(), false, false, codex_home.path(), false);

    assert_eq!(actual.as_bytes(), base.as_bytes());
}

#[test]
fn jit_feature_appends_jit_instructions_without_trim_policy() {
    let base = "base instructions".to_string();
    let codex_home = tempfile::tempdir().expect("tempdir");
    let actual =
        append_spine_view_instructions(base.clone(), true, false, codex_home.path(), false);

    assert!(actual.starts_with(&base));
    assert!(actual.len() > base.len());
    assert_eq!(occurrences(&actual, SPINE_JIT_INSTRUCTIONS), 1);
    assert!(!actual.contains("spine.trim"));
    assert!(!actual.contains("TRIM_ID"));
    assert_eq!(
        actual
            .strip_prefix(&base)
            .expect("base instructions prefix"),
        format!("\n\n{SPINE_JIT_INSTRUCTIONS}")
    );
}

#[test]
fn trim_feature_does_not_append_prompt_instructions() {
    let base = "base instructions".to_string();
    let codex_home = tempfile::tempdir().expect("tempdir");
    let actual =
        append_spine_view_instructions(base.clone(), false, true, codex_home.path(), false);

    assert_eq!(actual, base);
}

#[test]
fn jit_feature_does_not_append_spine_view_twice() {
    let base = format!("base instructions\n\n{SPINE_JIT_INSTRUCTIONS}");
    let codex_home = tempfile::tempdir().expect("tempdir");
    let actual =
        append_spine_view_instructions(base.clone(), true, false, codex_home.path(), false);

    assert_eq!(actual, base);
    assert_eq!(occurrences(&actual, SPINE_JIT_INSTRUCTIONS), 1);
}

#[test]
fn combined_features_append_only_jit_prompt_instructions() {
    let base = "base instructions".to_string();
    let codex_home = tempfile::tempdir().expect("tempdir");
    let actual = append_spine_view_instructions(base.clone(), true, true, codex_home.path(), false);

    assert_eq!(
        actual
            .strip_prefix(&base)
            .expect("base instructions prefix"),
        format!("\n\n{SPINE_JIT_INSTRUCTIONS}")
    );
}

#[test]
#[cfg(debug_assertions)]
fn feature_on_uses_spine_instruction_md_override_from_codex_home() {
    let codex_home = tempfile::tempdir().expect("tempdir");
    let override_instructions = "<spine_view>\nlocal override\n</spine_view>";
    std::fs::write(
        codex_home.path().join("spine_instruction.md"),
        override_instructions,
    )
    .expect("write override");

    let actual = append_spine_view_instructions(
        "base instructions".to_string(),
        true,
        true,
        codex_home.path(),
        true,
    );

    assert_eq!(
        actual,
        format!("base instructions\n\n{override_instructions}")
    );
    assert!(!actual.contains(SPINE_JIT_INSTRUCTIONS));
}

#[test]
#[cfg(debug_assertions)]
fn feature_on_uses_matching_override_sections_and_keeps_missing_enabled_sections() {
    let codex_home = tempfile::tempdir().expect("tempdir");
    let override_instructions = "<spine_view>\nlocal override\n</spine_view>";
    std::fs::write(
        codex_home.path().join("spine_instruction.md"),
        override_instructions,
    )
    .expect("write override");

    let base = format!("base instructions\n\n{SPINE_JIT_INSTRUCTIONS}");
    let actual = append_spine_view_instructions(base, true, true, codex_home.path(), true);

    assert_eq!(
        actual,
        format!("base instructions\n\n{override_instructions}")
    );
    assert!(!actual.contains(SPINE_JIT_INSTRUCTIONS));
}

#[test]
#[cfg(debug_assertions)]
fn feature_on_ignores_untagged_override_contents() {
    let codex_home = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        codex_home.path().join("spine_instruction.md"),
        "local untagged override",
    )
    .expect("write override");

    let actual = append_spine_view_instructions(
        "base instructions".to_string(),
        true,
        true,
        codex_home.path(),
        true,
    );

    assert_eq!(
        actual,
        format!("base instructions\n\n{SPINE_JIT_INSTRUCTIONS}")
    );
    assert!(!actual.contains("local untagged override"));
}

#[test]
#[cfg(not(debug_assertions))]
fn feature_on_ignores_spine_instruction_md_override_in_release_builds() {
    let codex_home = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        codex_home.path().join("spine_instruction.md"),
        "<spine_view>\nlocal override\n</spine_view>",
    )
    .expect("write override");

    let actual = append_spine_view_instructions(
        "base instructions".to_string(),
        true,
        true,
        codex_home.path(),
        true,
    );

    assert_eq!(
        actual,
        format!("base instructions\n\n{SPINE_JIT_INSTRUCTIONS}")
    );
}

#[test]
fn feature_on_ignores_spine_instruction_md_override_outside_dev_debug() {
    let codex_home = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        codex_home.path().join("spine_instruction.md"),
        "<spine_view>\nlocal override\n</spine_view>",
    )
    .expect("write override");

    let actual = append_spine_view_instructions(
        "base instructions".to_string(),
        true,
        true,
        codex_home.path(),
        false,
    );

    assert_eq!(
        actual,
        format!("base instructions\n\n{SPINE_JIT_INSTRUCTIONS}")
    );
}
