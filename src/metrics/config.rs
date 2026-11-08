//! Immutable metrics configuration, validated before binding.
use crate::{Error, Result};

/// Metrics settings. Compile the `metrics` feature to opt in.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
#[derive(serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MetricsConfig {
    pub enabled: bool,
    pub http_host: String,
    pub http_port: u16,
    pub http_server_threads: u16,
    /// Zero disables periodic log summaries.
    pub report_interval_seconds: u32,
}

impl Default for MetricsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            http_host: "0.0.0.0".into(),
            http_port: 9100,
            http_server_threads: 2,
            report_interval_seconds: 30,
        }
    }
}

impl MetricsConfig {
    pub(crate) fn validate(&self) -> Result<()> {
        let cfg = self;
        if cfg.http_port == 0 {
            return Err(Error::InvalidConfig("invalid metrics/http_port"));
        }
        if !(1..=64).contains(&cfg.http_server_threads) {
            return Err(Error::InvalidConfig(
                "metrics/http_server_threads outside 1..=64",
            ));
        }
        if !valid_host(&cfg.http_host) {
            return Err(Error::InvalidConfig("invalid metrics/http_host"));
        }
        Ok(())
    }
}

fn valid_host(host: &str) -> bool {
    if host.parse::<std::net::IpAddr>().is_ok() {
        return true;
    }
    let name = host.strip_suffix('.').unwrap_or(host);
    name.len() <= 253
        && name.bytes().any(|b| b.is_ascii_alphabetic())
        && name.split('.').all(|label| {
            (1..=63).contains(&label.len())
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
}
