use crate::spine::SPINE_TOOL_CLOSE;
use crate::spine::SPINE_TOOL_NEXT;
use crate::spine::SPINE_TOOL_OPEN;
use crate::spine::SPINE_TOOL_TREE;
use crate::spine::is_spine_close_like_tool_name;

#[test]
fn close_like_tool_name_filters_only_close_and_next() {
    assert!(is_spine_close_like_tool_name(SPINE_TOOL_CLOSE));
    assert!(is_spine_close_like_tool_name(SPINE_TOOL_NEXT));
    assert!(!is_spine_close_like_tool_name(SPINE_TOOL_OPEN));
    assert!(!is_spine_close_like_tool_name(SPINE_TOOL_TREE));
}
