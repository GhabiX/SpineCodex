use super::*;

#[test]
fn spine_error_classifies_fail_closed_boundaries() {
    let tool_use = SpineError::ToolUse("bad tool args".to_string());
    assert_eq!(tool_use.class(), SpineErrorClass::ToolUse);
    assert!(!tool_use.should_invalidate_runtime());

    let operation = SpineError::Operation("bad operation order".to_string());
    assert_eq!(operation.class(), SpineErrorClass::Operation);
    assert!(!operation.should_invalidate_runtime());

    let compact = SpineError::CompactFailure("compact failed before commit".to_string());
    assert_eq!(compact.class(), SpineErrorClass::CompactFailure);
    assert!(!compact.should_invalidate_runtime());

    let invariant = SpineError::Invariant("committed state mismatch".to_string());
    assert_eq!(invariant.class(), SpineErrorClass::Invariant);
    assert!(invariant.should_invalidate_runtime());

    let corruption = SpineError::SidecarCorruption("missing sidecar evidence".to_string());
    assert_eq!(corruption.class(), SpineErrorClass::SidecarCorruption);
    assert!(corruption.should_invalidate_runtime());
}
