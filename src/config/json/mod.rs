//! OrbiTE configuration: a bounded immutable snapshot, never a process override.
mod document;
use super::EngineConfig;
use crate::{Error, Result};
use serde::de::DeserializeOwned;
#[cfg(feature = "rdma")]
use serde::{Deserialize, Deserializer};
use serde_json::Value;
use std::{path::Path, time::Duration};

fn invalid(path: &str, reason: impl Into<String>) -> Error {
    Error::Config {
        path: path.into(),
        reason: reason.into(),
    }
}
fn field<T: DeserializeOwned>(path: &str, value: Value) -> Result<T> {
    serde_json::from_value(value).map_err(|e| invalid(path, e.to_string()))
}
fn object_field<T: DeserializeOwned>(path: &str, value: Value) -> Result<T> {
    if !value.is_object() {
        return Err(invalid(path, "expected an object"));
    }
    field(path, value)
}
#[cfg(feature = "rdma")]
pub(crate) fn duration<'de, D: Deserializer<'de>>(d: D) -> std::result::Result<Duration, D::Error> {
    u64::deserialize(d).map(Duration::from_micros)
}
impl EngineConfig {
    /// Parse an OrbiTE JSON object (at most 1 MiB). Explicit input never reads
    /// ORBITE_CONF. Unknown keys, duplicate keys and wrong types are errors.
    pub fn from_json(json: &str) -> Result<Self> {
        let mut root = document::parse(json, "$")?;
        let fields = root
            .as_object_mut()
            .ok_or_else(|| invalid("$", "expected an object"))?;
        // Optional grouping of transport settings. A field cannot occur twice.
        if let Some(group) = fields.remove("transports") {
            let Value::Object(group) = group else {
                return Err(invalid("transports", "expected an object"));
            };
            for (key, value) in group {
                if !matches!(
                    key.as_str(),
                    "tcp"
                        | "hp_tcp"
                        | "shm"
                        | "rdma"
                        | "io_uring"
                        | "io_uring_enabled"
                        | "nvlink"
                        | "nvlink_async_memcpy_threshold_kb"
                        | "nvlink_cache_entries"
                        | "nvlink_stream_count"
                        | "mnnvl"
                        | "gds_enabled"
                        | "gds_batch_depth"
                        | "gds_require_native"
                ) {
                    return Err(invalid(
                        &format!("transports/{key}"),
                        "unknown transport setting",
                    ));
                }
                if fields.insert(key.clone(), value).is_some() {
                    return Err(invalid(&key, "duplicate transport setting"));
                }
            }
        }
        let mut config = Self::default();
        for (key, value) in std::mem::take(fields) {
            match key.as_str() {
                "tcp" => config.tcp = field(&key, value)?,
                "hp_tcp" => config.hp_tcp = object_field(&key, value)?,
                #[cfg(feature = "io-uring")]
                "io_uring" => config.io_uring = object_field(&key, value)?,
                #[cfg(feature = "io-uring")]
                "io_uring_enabled" => config.io_uring_enabled = field(&key, value)?,
                "shm" => config.shm = object_field(&key, value)?,
                #[cfg(feature = "metrics")]
                "metrics" => config.metrics = object_field(&key, value)?,
                "merge_requests" => config.merge_requests = field(&key, value)?,
                "max_failover_attempts" => config.max_failover_attempts = field(&key, value)?,
                "enable_auto_failover_on_poll" => {
                    config.enable_auto_failover_on_poll = field(&key, value)?
                }
                #[cfg(feature = "gds")]
                "gds_batch_depth" => config.gds_batch_depth = field(&key, value)?,
                #[cfg(feature = "gds")]
                "gds_require_native" => config.gds_require_native = field(&key, value)?,
                #[cfg(feature = "gds")]
                "gds_enabled" => config.gds_enabled = field(&key, value)?,
                #[cfg(feature = "cuda")]
                "nvlink" => config.nvlink = field(&key, value)?,
                #[cfg(feature = "cuda")]
                "nvlink_async_memcpy_threshold_kb" => {
                    config.nvlink_async_memcpy_threshold_kb = field(&key, value)?
                }
                #[cfg(feature = "cuda")]
                "mnnvl" => {
                    config.mnnvl = if value.is_null() {
                        None
                    } else {
                        Some(object_field(&key, value)?)
                    }
                }
                #[cfg(feature = "cuda")]
                "nvlink_cache_entries" => config.nvlink_cache_entries = field(&key, value)?,
                #[cfg(feature = "cuda")]
                "nvlink_stream_count" => config.nvlink_stream_count = field(&key, value)?,
                #[cfg(feature = "cuda")]
                "staging_chunk_size" => config.staging_chunk_size = field(&key, value)?,
                #[cfg(feature = "cuda")]
                "staging_chunk_count" => config.staging_chunk_count = field(&key, value)?,
                #[cfg(feature = "cuda")]
                "staging_location" => config.staging_location = field(&key, value)?,
                #[cfg(feature = "cuda")]
                "staging_pipeline_depth" => config.staging_pipeline_depth = field(&key, value)?,
                #[cfg(feature = "cuda")]
                "remote_staging" => config.remote_staging = field(&key, value)?,
                #[cfg(feature = "rdma")]
                "rdma" => {
                    config.rdma = if value.is_null() {
                        None
                    } else {
                        Some(object_field(&key, value)?)
                    }
                }
                "bind_addr" => config.bind_addr = field(&key, value)?,
                "max_registered_regions" => config.max_registered_regions = field(&key, value)?,
                "max_registered_bytes" => config.max_registered_bytes = field(&key, value)?,
                "max_file_segments" => config.max_file_segments = field(&key, value)?,
                "max_peer_segments" => config.max_peer_segments = field(&key, value)?,
                "max_batch_requests" => config.max_batch_requests = field(&key, value)?,
                "max_allocated_batches" => config.max_allocated_batches = field(&key, value)?,
                "max_allocated_requests" => config.max_allocated_requests = field(&key, value)?,
                "enable_progress_worker" => config.enable_progress_worker = field(&key, value)?,
                "progress_fallback_interval_us" => {
                    config.progress_fallback_interval = Duration::from_micros(field(&key, value)?)
                }
                "notification_rpc_fallback" => {
                    config.notification_rpc_fallback = field(&key, value)?
                }
                "tcp_notifications" => config.tcp_notifications = field(&key, value)?,
                "runtime_queue" => {
                    config.runtime_queue = if value.is_null() {
                        None
                    } else {
                        Some(object_field(&key, value)?)
                    }
                }
                "qos" => {
                    config.qos =
                        crate::QosConfig::from_json(&serde_json::json!({"qos": value}).to_string())?
                }
                "selector" => {
                    config.selector = if value.is_null() {
                        None
                    } else {
                        Some(crate::TransportSelectorConfig::from_json(
                            &value.to_string(),
                        )?)
                    }
                }
                "max_receiver_credit_sessions" => {
                    config.max_receiver_credit_sessions = field(&key, value)?
                }
                "receiver_credit" => {
                    config.receiver_credit = if value.is_null() {
                        None
                    } else {
                        Some(object_field(&key, value)?)
                    }
                }
                "max_inflight_batches" => config.max_inflight_batches = field(&key, value)?,
                "max_inflight_bytes" => config.max_inflight_bytes = field(&key, value)?,
                "max_transfer_bytes" => config.max_transfer_bytes = field(&key, value)?,
                "max_pending_notifications" => {
                    config.max_pending_notifications = field(&key, value)?
                }
                "max_pending_notification_bytes" => {
                    config.max_pending_notification_bytes = field(&key, value)?
                }
                "max_notification_bytes" => config.max_notification_bytes = field(&key, value)?,
                "handler_threads" => config.handler_threads = field(&key, value)?,
                "handler_timeout_us" => {
                    config.handler_timeout = Duration::from_micros(field(&key, value)?)
                }
                _ => {
                    return Err(invalid(
                        &key,
                        "unknown configuration field (or feature not built)",
                    ));
                }
            }
        }
        #[cfg(any(feature = "rdma", feature = "io-uring", feature = "metrics"))]
        let config = config.resolve_subsystems()?;
        config.validate()?;
        Ok(config)
    }
    /// Read a bounded, regular UTF-8 JSON file. Does not consult the environment.
    pub fn from_file(path: impl AsRef<Path>) -> Result<Self> {
        Self::from_json(&document::read(path.as_ref(), "file")?)
    }
    /// Read only ORBITE_CONF: inline JSON or a file path. When absent, return
    /// typed defaults. The resulting snapshot never rereads the environment.
    pub fn from_env() -> Result<Self> {
        let Some(input) = std::env::var_os("ORBITE_CONF") else {
            return Ok(Self::default());
        };
        let input = input
            .to_str()
            .ok_or_else(|| invalid("ORBITE_CONF", "expected UTF-8"))?;
        if input.len() > document::MAX_BYTES || input.trim().is_empty() {
            return Err(invalid(
                "ORBITE_CONF",
                "expected a path or JSON within 1 MiB",
            ));
        }
        if input.trim_start().starts_with(['{', '[']) {
            Self::from_json(input)
        } else {
            Self::from_file(input)
        }
    }
}
#[cfg(feature = "rdma")]
impl crate::transport::rdma::RdmaConfig {
    pub(in crate::config) fn capture_topology_file(&mut self) -> Result<()> {
        if self.priority_matrix.is_none() && self.custom_topology_json.is_none() {
            if let Some(path) = &self.custom_json_path {
                self.custom_topology_json = Some(document::read(path, "rdma/custom_json_path")?);
            }
        }
        Ok(())
    }
}
#[cfg(test)]
#[path = "../../../tests/unit/config/json.rs"]
mod tests;
