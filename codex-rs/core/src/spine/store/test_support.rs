use super::SpineStore;
use crate::spine::SpineError;
use crate::spine::model::SpineLedgerEvent;

impl SpineStore {
    pub(crate) fn event_count_for_test(&self) -> Result<usize, SpineError> {
        Ok(self.events()?.len())
    }

    pub(crate) fn suffix_mem_cover_for_test(
        &self,
        node_path: &str,
    ) -> Result<Option<(u64, u64, usize, usize)>, SpineError> {
        Ok(self
            .mems()?
            .into_iter()
            .find(|mem| mem.node.as_path() == node_path)
            .map(|mem| {
                (
                    mem.raw_start,
                    mem.raw_end,
                    mem.context_start,
                    mem.context_end,
                )
            }))
    }

    pub(crate) fn memory_body_for_test(
        &self,
        node_path: &str,
    ) -> Result<Option<String>, SpineError> {
        self.mems()?
            .into_iter()
            .find(|mem| mem.node.as_path() == node_path)
            .map(|mem| self.read_memory_body(&mem))
            .transpose()
    }

    pub(crate) fn mem_close_tokens_for_test(
        &self,
    ) -> Result<Vec<(Option<i64>, Option<i64>)>, SpineError> {
        Ok(self
            .mems()?
            .into_iter()
            .map(|mem| (mem.close_input_tokens, mem.close_context_tokens))
            .collect())
    }

    pub(crate) fn root_compact_next_open_tokens_for_test(
        &self,
    ) -> Result<Vec<(Option<i64>, Option<i64>)>, SpineError> {
        Ok(self
            .events()?
            .into_iter()
            .filter_map(|event| match event.event {
                SpineLedgerEvent::RootCompact {
                    next_open_input_tokens,
                    next_open_context_tokens,
                    ..
                } => Some((next_open_input_tokens, next_open_context_tokens)),
                _ => None,
            })
            .collect())
    }
}
