use super::*;
use pretty_assertions::assert_eq;

fn auto_section(base: &str, rollout: &str, body: &str, summary: &str) -> String {
    format!(
        "\n\n## Auto Compact\n\nBase: {base}\nFold: response ordinals [2, 20)\nNode trajs: nodes/1/trajs.jsonl\nRaw mirror: raw/rollout.raw.jsonl\nRollout: {rollout}\n\n{body}\n\n## Node Summary\n\n{summary}\n"
    )
}

#[test]
fn mem_install_parses_indexed_generated_sections() {
    let first = auto_section("/old/base", "/old/rollout.jsonl", "first facts", "first");
    let second = auto_section("/old/base", "/old/rollout.jsonl", "second facts", "second");
    let memory = format!(
        "manual note{GENERATED_MEMORY_SECTION_MARKER}{first}{GENERATED_MEMORY_SECTION_MARKER}{second}"
    );

    let sections = parse_generated_memory_sections("nodes/1/memory.md", &memory);

    assert_eq!(
        sections,
        vec![
            GeneratedMemorySection {
                section_id: MemorySectionId::new("nodes/1/memory.md", 0),
                payload: first,
                body: "first facts".to_string(),
                body_hash: memory_body_hash("first facts"),
            },
            GeneratedMemorySection {
                section_id: MemorySectionId::new("nodes/1/memory.md", 1),
                payload: second,
                body: "second facts".to_string(),
                body_hash: memory_body_hash("second facts"),
            },
        ]
    );
}

#[test]
fn mem_install_verifies_body_ref_by_section_index() {
    let first = auto_section("/base", "/rollout.jsonl", "first facts", "first");
    let second = auto_section("/base", "/rollout.jsonl", "second facts", "second");
    let memory = format!(
        "{GENERATED_MEMORY_SECTION_MARKER}{first}{GENERATED_MEMORY_SECTION_MARKER}{second}"
    );
    let sections = parse_generated_memory_sections("nodes/1/memory.md", &memory);
    let second_ref = sections[1].body_ref();

    assert_eq!(
        verify_memory_body_ref("nodes/1/memory.md", &memory, &second_ref).expect("verify second"),
        sections[1]
    );

    let wrong_hash = MemoryBodyRef {
        section_id: MemorySectionId::new("nodes/1/memory.md", 1),
        body_hash: memory_body_hash("first facts"),
    };
    assert!(matches!(
        verify_memory_body_ref("nodes/1/memory.md", &memory, &wrong_hash),
        Err(MemoryBodyError::BodyHashMismatch { .. })
    ));

    let wrong_storage = MemoryBodyRef {
        section_id: MemorySectionId::new("nodes/2/memory.md", 1),
        body_hash: second_ref.body_hash,
    };
    assert!(matches!(
        verify_memory_body_ref("nodes/1/memory.md", &memory, &wrong_storage),
        Err(MemoryBodyError::StorageMismatch { .. })
    ));
}

#[test]
fn mem_install_hash_ignores_auto_compact_audit_path_drift() {
    let source = format!(
        "{GENERATED_MEMORY_SECTION_MARKER}{}",
        auto_section(
            "/parent/base",
            "/parent/rollout.jsonl",
            "portable facts",
            "done"
        )
    );
    let relocated = source
        .replace("/parent/base", "/child/base")
        .replace("/parent/rollout.jsonl", "/child/rollout.jsonl");

    let source_section = &parse_generated_memory_sections("nodes/1/memory.md", &source)[0];
    let relocated_section = &parse_generated_memory_sections("nodes/1/memory.md", &relocated)[0];

    assert_eq!(source_section.body, "portable facts");
    assert_eq!(source_section.body_hash, relocated_section.body_hash);
}

#[test]
fn mem_install_hashes_plain_section_payload_when_shape_is_unknown() {
    let memory = format!("{GENERATED_MEMORY_SECTION_MARKER}plain generated payload");

    let sections = parse_generated_memory_sections("nodes/1/memory.md", &memory);

    assert_eq!(sections[0].body, "plain generated payload");
    assert_eq!(
        sections[0].body_hash,
        memory_body_hash("plain generated payload")
    );
}
