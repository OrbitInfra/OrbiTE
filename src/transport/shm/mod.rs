//! Same-host shared mappings. Describe carries allocation identity and offsets;
//! the source retains authority for each individual registration.
#![deny(missing_docs)]

use std::path::PathBuf;

/// Linux shared-memory transport settings, validated at engine construction.
/// Mappings stay retained by copies until their source lease is released.
#[derive(Clone, Debug, Default, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
pub struct ShmConfig {
    /// Opt in to SHM; false by default. Enabling on non-Linux returns Unavailable.
    pub enable: bool,
    /// Empty selects POSIX shared memory. Otherwise both peers specify their local mount.
    /// Default empty; otherwise must be absolute at construction. No CXL capability is inferred.
    pub cxl_mount_path: PathBuf,
}

#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
mod mapping;
#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
mod sys;
#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
mod transport;
pub(crate) mod wire;
#[cfg(target_os = "linux")]
pub(crate) use mapping::{Local, RangeLock};
#[cfg(target_os = "linux")]
pub(crate) use transport::{Lease, ShmTransport};

#[cfg(all(test, target_os = "linux"))]
#[path = "../../../tests/unit/transport/shm/mod.rs"]
mod tests;
