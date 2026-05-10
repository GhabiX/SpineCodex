use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct NodeId(Vec<u32>);

impl NodeId {
    pub(crate) fn root() -> Self {
        Self(vec![1])
    }

    pub(crate) fn from_segments(segments: Vec<u32>) -> Self {
        Self(segments)
    }

    pub(crate) fn root_sibling(index: u32) -> Self {
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

impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, segment) in self.0.iter().enumerate() {
            if index > 0 {
                f.write_str(".")?;
            }
            write!(f, "{segment}")?;
        }
        Ok(())
    }
}
