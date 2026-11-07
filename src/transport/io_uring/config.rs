use crate::{Error, Result};

#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
#[derive(serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct IoUringConfig {
    pub pinned_staging: bool,
    /// KiB per slot, rounded up to a 4 KiB boundary.
    pub staging_slot_kb: usize,
    pub staging_slots: usize,
}
impl Default for IoUringConfig {
    fn default() -> Self {
        Self {
            pinned_staging: false,
            staging_slot_kb: 4096,
            staging_slots: 32,
        }
    }
}
impl IoUringConfig {
    pub(crate) fn slot_size(self) -> Result<usize> {
        if !self.pinned_staging {
            return Ok(0);
        }
        let size = self
            .staging_slot_kb
            .checked_mul(1024)
            .and_then(|n| n.checked_add(4095))
            .map(|n| n & !4095);
        size.filter(|&n| {
            n > 0
                && (1..=4096).contains(&self.staging_slots)
                && n.checked_mul(self.staging_slots)
                    .is_some_and(|b| b <= 1 << 30)
        })
        .ok_or(Error::InvalidConfig(
            "io_uring staging slots must be positive and total at most 1 GiB",
        ))
    }
}
