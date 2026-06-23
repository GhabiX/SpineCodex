use crate::spine::SpineError;
use crate::spine::io::hash_raw_live;
use crate::spine::io::hash_raw_live_prefix_all_true;

#[derive(Clone, Copy)]
pub(in crate::spine) struct RawMask<'a> {
    live: Option<&'a [bool]>,
}

impl<'a> RawMask<'a> {
    pub(in crate::spine) fn new(live: &'a [bool]) -> Self {
        Self { live: Some(live) }
    }

    pub(in crate::spine) fn boundary_live(self, boundary: u64) -> Result<bool, SpineError> {
        let Some(live) = self.live else {
            return Ok(true);
        };
        if boundary == 0 {
            return Ok(true);
        }
        let index = raw_usize(boundary - 1, "raw boundary overflow")?;
        Ok(live.get(index).copied().unwrap_or(false))
    }

    pub(in crate::spine) fn raw_index_live(self, index: u64) -> Result<bool, SpineError> {
        let Some(live) = self.live else {
            return Ok(true);
        };
        let index = raw_usize(index, "raw index overflow")?;
        Ok(live.get(index).copied().unwrap_or(false))
    }

    pub(in crate::spine) fn span_live(self, start: u64, end: u64) -> Result<bool, SpineError> {
        let Some(live) = self.live else {
            return Ok(true);
        };
        let start = raw_usize(start, "raw start overflow")?;
        let end = raw_usize(end, "raw end overflow")?;
        if end > live.len() || start > end {
            return Ok(false);
        }
        Ok(live[start..end].iter().all(|item| *item))
    }

    pub(in crate::spine) fn prefix_hash_matches(
        self,
        end: u64,
        expected: &str,
    ) -> Result<bool, SpineError> {
        let end = raw_usize(end, "raw end overflow")?;
        let Some(live) = self.live else {
            return Ok(hash_raw_live_prefix_all_true(end) == expected);
        };
        if end > live.len() {
            return Ok(false);
        }
        Ok(hash_raw_live(&live[..end]) == expected)
    }
}

fn raw_usize(value: u64, overflow_message: &str) -> Result<usize, SpineError> {
    usize::try_from(value).map_err(|_| SpineError::InvalidEvent(overflow_message.to_string()))
}
