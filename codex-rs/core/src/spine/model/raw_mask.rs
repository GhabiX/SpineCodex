use crate::spine::SpineError;
use crate::spine::io::hash_raw_live;

#[derive(Clone, Copy)]
pub(in crate::spine) struct RawMask<'a> {
    live: &'a [bool],
}

impl<'a> RawMask<'a> {
    pub(in crate::spine) fn new(live: &'a [bool]) -> Self {
        Self { live }
    }

    pub(in crate::spine) fn boundary_live(self, boundary: u64) -> Result<bool, SpineError> {
        if boundary == 0 {
            return Ok(true);
        }
        let index = raw_usize(boundary - 1, "raw boundary overflow")?;
        Ok(self.live.get(index).copied().unwrap_or(false))
    }

    pub(in crate::spine) fn raw_index_live(self, index: u64) -> Result<bool, SpineError> {
        let index = raw_usize(index, "raw index overflow")?;
        Ok(self.live.get(index).copied().unwrap_or(false))
    }

    pub(in crate::spine) fn span_live(self, start: u64, end: u64) -> Result<bool, SpineError> {
        let start = raw_usize(start, "raw start overflow")?;
        let end = raw_usize(end, "raw end overflow")?;
        Ok(self
            .live
            .get(start..end)
            .is_some_and(|span| span.iter().all(|item| *item)))
    }

    pub(in crate::spine) fn prefix_hash_matches(
        self,
        end: u64,
        expected: &str,
    ) -> Result<bool, SpineError> {
        self.prefix_hash_matches_with_overflow(end, expected, "raw end overflow")
    }

    pub(in crate::spine) fn prefix_hash_matches_with_overflow(
        self,
        end: u64,
        expected: &str,
        overflow_message: &str,
    ) -> Result<bool, SpineError> {
        let end = raw_usize(end, overflow_message)?;
        Ok(self
            .live
            .get(..end)
            .is_some_and(|prefix| hash_raw_live(prefix) == expected))
    }
}

fn raw_usize(value: u64, overflow_message: &str) -> Result<usize, SpineError> {
    usize::try_from(value).map_err(|_| SpineError::InvalidEvent(overflow_message.to_string()))
}
