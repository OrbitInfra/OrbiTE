//! Immutable metrics configuration, validated before binding.
use crate::{Error, Result};

/// Metrics settings. Compile the `metrics` feature to opt in.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
#[derive(serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MetricsConfig {
    /// Create metrics recording and services for this engine; defaults to true.
    pub enabled: bool,
    /// HTTP bind IP or hostname; defaults to `0.0.0.0`. No authentication is added.
    pub http_host: String,
    /// Nonzero HTTP port; defaults to 9100. Bind failure leaves log-only metrics.
    pub http_port: u16,
    /// Bounded HTTP service threads, 1..=64; defaults to two.
    pub http_server_threads: u16,
    /// Log-summary interval in seconds; defaults to 30, zero disables periodic reports.
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
