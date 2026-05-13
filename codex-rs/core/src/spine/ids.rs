use std::fmt;
use std::str::FromStr;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct NodeId(Vec<u32>);

impl NodeId {
    pub(crate) fn root() -> Self {
        Self(Vec::new())
    }

    pub(crate) fn from_segments(segments: Vec<u32>) -> Self {
        Self(segments)
    }

    pub(crate) fn parse(value: &str) -> Result<Self, NodeIdParseError> {
        value.parse()
    }

    pub(crate) fn root_epoch(index: u32) -> Self {
        Self(vec![index])
    }

    pub(crate) fn child(&self, index: u32) -> Self {
        let mut segments = self.0.clone();
        segments.push(index);
        Self(segments)
    }

    pub(crate) fn segments(&self) -> &[u32] {
        &self.0
    }

    pub(crate) fn bracketed(&self) -> String {
        format!("[{self}]")
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum NodeIdParseError {
    Empty,
    EmptySegment,
    InvalidSegment(String),
    ZeroSegment,
}

impl fmt::Display for NodeIdParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NodeIdParseError::Empty => f.write_str("spine node id must not be empty"),
            NodeIdParseError::EmptySegment => {
                f.write_str("spine node id segments must not be empty")
            }
            NodeIdParseError::InvalidSegment(segment) => {
                write!(f, "invalid spine node id segment {segment:?}")
            }
            NodeIdParseError::ZeroSegment => {
                f.write_str("spine node id segments must be greater than zero")
            }
        }
    }
}

impl std::error::Error for NodeIdParseError {}

impl FromStr for NodeId {
    type Err = NodeIdParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.is_empty() {
            return Err(NodeIdParseError::Empty);
        }

        let mut segments = Vec::new();
        for segment in value.split('.') {
            if segment.is_empty() {
                return Err(NodeIdParseError::EmptySegment);
            }
            let segment = segment
                .parse::<u32>()
                .map_err(|_| NodeIdParseError::InvalidSegment(segment.to_string()))?;
            if segment == 0 {
                return Err(NodeIdParseError::ZeroSegment);
            }
            segments.push(segment);
        }

        Ok(Self(segments))
    }
}

impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0.is_empty() {
            return f.write_str("root");
        }
        for (index, segment) in self.0.iter().enumerate() {
            if index > 0 {
                f.write_str(".")?;
            }
            write!(f, "{segment}")?;
        }
        Ok(())
    }
}
