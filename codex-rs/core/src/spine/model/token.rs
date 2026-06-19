use serde::Deserialize;
use serde::Serialize;
use std::fmt;
use std::ops::Range;
use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub(crate) struct NodeId(pub(in crate::spine) Vec<u32>);

impl NodeId {
    pub(in crate::spine) fn root_epoch(index: u32) -> Self {
        Self(vec![index])
    }

    pub(in crate::spine) fn child(&self, index: u32) -> Self {
        let mut path = self.0.clone();
        path.push(index);
        Self(path)
    }

    pub(in crate::spine) fn parent(&self) -> Option<Self> {
        (self.0.len() > 1).then(|| Self(self.0[..self.0.len() - 1].to_vec()))
    }

    pub(in crate::spine) fn is_root_epoch(&self) -> bool {
        self.0.len() == 1
    }

    pub(in crate::spine) fn as_path(&self) -> String {
        self.0
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(".")
    }
}

impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.as_path())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(in crate::spine) enum NodeStatus {
    Live,
    Opened,
    Closed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(in crate::spine) enum ContextBaselineSource {
    ProviderAtOpen,
    RootCompactHandoff,
    EstimatedFromLiveSuffix,
    CheckpointReplay,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(in crate::spine) struct TreeMeta {
    pub(in crate::spine) id: NodeId,
    pub(in crate::spine) index: usize,
    pub(in crate::spine) summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(in crate::spine) open_input_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(in crate::spine) open_context_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(in crate::spine) open_context_source: Option<ContextBaselineSource>,
    pub(in crate::spine) node_dir: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(in crate::spine) enum SegRef {
    ResponseItem {
        raw_ordinal: u64,
        context_index: usize,
    },
    Memory {
        memory_id: String,
        body_path: PathBuf,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ToolCallSegmentKind {
    Request,
    Response,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(in crate::spine) struct ToolCallEventSegment {
    pub(in crate::spine) kind: ToolCallSegmentKind,
    pub(in crate::spine) raw_ordinal: u64,
    pub(in crate::spine) context_index: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(in crate::spine) struct ToolCallSegment {
    pub(in crate::spine) kind: ToolCallSegmentKind,
    pub(in crate::spine) seg: SegRef,
}

impl SegRef {
    #[cfg(test)]
    pub(in crate::spine) fn from_memory_ref(memory: &MemoryRef) -> Self {
        Self::Memory {
            memory_id: memory.compact_id.clone(),
            body_path: memory.body_path.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(in crate::spine) struct MemoryRef {
    pub(in crate::spine) compact_id: String,
    pub(in crate::spine) node_id: NodeId,
    pub(in crate::spine) body_path: PathBuf,
    pub(in crate::spine) body_hash: String,
    pub(in crate::spine) source_raw_range: Range<u64>,
    pub(in crate::spine) source_context_range: Range<usize>,
    pub(in crate::spine) source_token_seq: Range<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(in crate::spine) open_input_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(in crate::spine) close_input_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(in crate::spine) open_context_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(in crate::spine) close_context_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(in crate::spine) closed_source_suffix_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(in crate::spine) closed_memory_context_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(in crate::spine) open_context_source: Option<ContextBaselineSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(in crate::spine) memory_output_tokens: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(in crate::spine) enum SpineToken {
    Init {
        meta: TreeMeta,
    },
    End,
    Open {
        meta: TreeMeta,
    },
    Close {
        memory: MemoryRef,
    },
    Compact {
        memory: MemoryRef,
        next_open_index: usize,
        next_open_input_tokens: Option<i64>,
        next_open_context_tokens: Option<i64>,
    },
    Msg {
        seg: SegRef,
        from_user: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        user_anchor: Option<u64>,
    },
    ToolCall {
        segments: Vec<ToolCallSegment>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(in crate::spine) enum ControlSymbol {
    Init(TreeMeta),
    End,
    Open(TreeMeta),
    Close(MemoryRef),
    Compact(MemoryRef, usize, Option<i64>, Option<i64>),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(in crate::spine) enum Symbol {
    Control(ControlSymbol),
    SpineTreeNode(SpineTreeNode),
    SpineTreeNodes(Vec<SpineTreeNode>),
    RootEpoches(Vec<RootEpoch>),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(in crate::spine) enum SpineTreeNode {
    MsgAsLeafNode {
        msg: SegRef,
        from_user: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        user_anchor: Option<u64>,
    },
    ToolCallAsLeafNode {
        segments: Vec<ToolCallSegment>,
    },
    SpineTree {
        memory: MemoryRef,
        meta: TreeMeta,
        children: Vec<SpineTreeNode>,
        memory_path: PathBuf,
        trajs_path: PathBuf,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(in crate::spine) struct RootEpoch {
    pub(in crate::spine) memory: MemoryRef,
}
