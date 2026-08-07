#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SpinetreeMemoryProjectionEntry {
    pub(crate) node_id: String,
    pub(crate) summary: String,
    pub(crate) body: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SpinetreeUserMessageProjectionEntry {
    pub(crate) anchor: u64,
    pub(crate) body: String,
}
