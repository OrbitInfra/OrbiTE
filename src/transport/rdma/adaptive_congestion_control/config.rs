//! Startup configuration for the optional TENT shared RDMA congestion core.
use crate::{Error, Result};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum Mode {
    #[default]
    Off,
    Observe,
    Enforce,
}

#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct Config {
    pub mode: Mode,
    pub min_window_bytes: u64,
    pub max_window_bytes: u64,
    /// Retained for compatibility with the release's startup configuration.
    pub target_drain_time_ns: u64,
    pub high_pressure_epochs: u32,
    pub low_pressure_epochs: u32,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            mode: Mode::Off,
            min_window_bytes: 4 << 20,
            max_window_bytes: 64 << 20,
            target_drain_time_ns: 2_000_000,
            high_pressure_epochs: 2,
            low_pressure_epochs: 3,
        }
    }
}

impl Config {
    pub(super) fn validate(self) -> Result<()> {
        if self.min_window_bytes == 0 || self.min_window_bytes > self.max_window_bytes {
            return Err(Error::InvalidConfig("adaptive congestion window range"));
        }
        if self.target_drain_time_ns == 0 {
            return Err(Error::InvalidConfig("target_drain_time_ns"));
        }
        if self.high_pressure_epochs == 0 {
            return Err(Error::InvalidConfig("high_pressure_epochs"));
        }
        if self.low_pressure_epochs == 0 {
            return Err(Error::InvalidConfig("low_pressure_epochs"));
        }
        Ok(())
    }
}
