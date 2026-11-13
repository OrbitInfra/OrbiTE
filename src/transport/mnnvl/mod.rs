//! Multi-node NVLink capability and cooperative relocation.
#![deny(missing_docs)]

#[cfg(feature = "cuda")]
mod runtime;
use crate::{Error, MemoryType, Result, SourceEpoch};

/// CUDA mapping export format, selected at construction.
/// This enum is non-exhaustive; handle unknown variants.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
#[derive(serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MnnvlHandleType {
    #[default]
    /// Fabric handle for cooperating devices in a declared domain; the default.
    Fabric,
    /// POSIX descriptor handle restricted to cooperating processes on one host.
    PosixFd,
}

/// Opt-in multi-node NVLink mappings. Selecting this profile replaces legacy IPC;
/// imported mappings remain owned until their copies physically complete.
#[derive(Clone, Debug)]
#[non_exhaustive]
#[derive(serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MnnvlConfig {
    /// Shareable mapping handle; Fabric by default. PosixFd is restricted to one host.
    pub handle_type: MnnvlHandleType,
    /// Register ordinary host allocations with CUDA; true by default.
    /// Already-owned pinned allocations retain their existing registration.
    pub host_register: bool,
    /// Enable host-NUMA VMM allocations; false by default. Copies use the owning socket's GPU.
    pub egm: bool,
    /// Same fabric/IMEX domain on cooperating hosts. None restricts placement to this boot.
    #[serde(deserialize_with = "domain")]
    pub fabric_domain: Option<SourceEpoch>,
    /// Threshold in units of 128 KiB; defaults to 1024 (128 MiB), with checked scaling.
    /// Currently does not change dispatch: all sizes use asynchronous copies and event completion.
    pub async_memcpy_threshold: usize,
}
fn domain<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> std::result::Result<Option<SourceEpoch>, D::Error> {
    use serde::Deserialize;
    Option::<String>::deserialize(d)?
        .map(|s| s.parse().map_err(serde::de::Error::custom))
        .transpose()
}
impl Default for MnnvlConfig {
    fn default() -> Self {
        Self {
            handle_type: MnnvlHandleType::Fabric,
            host_register: true,
            egm: false,
            fabric_domain: None,
            async_memcpy_threshold: 1024,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Location {
    Device,
    Host(u32),
}
impl Location {
    pub(crate) fn wire(location: Option<Self>) -> u32 {
        match location {
            None => 0,
            Some(Self::Device) => 1,
            Some(Self::Host(node)) => 0x8000_0000 | node,
        }
    }
    pub(crate) fn decode(
        value: u32,
        memory: MemoryType,
        caps: Option<Capability>,
    ) -> Result<Option<Self>> {
        match (value, memory, caps) {
            (0, _, _) => Ok(None),
            (1, MemoryType::Cuda, Some(_)) => Ok(Some(Self::Device)),
            (n, MemoryType::Cpu, Some(c)) if c.egm && n & 0x8000_0000 != 0 => {
                Ok(Some(Self::Host(n & 0x7fff_ffff)))
            }
            _ => Err(Error::Protocol("invalid MNNVL registration location")),
        }
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Capability {
    pub domain: SourceEpoch,
    pub kind: MnnvlHandleType,
    pub egm: bool,
}
impl Capability {
    pub const SIZE: usize = 24;
    pub fn encode(self, b: &mut [u8]) {
        b[..16].copy_from_slice(&self.domain.0);
        b[16] = u8::from(self.kind == MnnvlHandleType::PosixFd);
        b[17] = u8::from(self.egm);
        b[18..Self::SIZE].fill(0);
    }
    pub fn decode(b: &[u8]) -> Result<Self> {
        if b.len() < Self::SIZE
            || b[..16] == [0; 16]
            || b[16] > 1
            || b[17] > 1
            || b[18..Self::SIZE] != [0; 6]
        {
            return Err(Error::Protocol("invalid MNNVL capability"));
        }
        let mut domain = [0; 16];
        domain.copy_from_slice(&b[..16]);
        Ok(Self {
            domain: SourceEpoch(domain),
            kind: if b[16] == 0 {
                MnnvlHandleType::Fabric
            } else {
                MnnvlHandleType::PosixFd
            },
            egm: b[17] == 1,
        })
    }
    pub fn reaches(self, peer: Self, same_host: bool, location: Location) -> bool {
        self.kind == peer.kind
            && self.domain == peer.domain
            && (self.kind == MnnvlHandleType::Fabric || same_host)
            && (location == Location::Device || (self.egm && peer.egm))
    }
}

#[cfg(test)]
#[path = "../../../tests/unit/transport/mnnvl.rs"]
mod tests;
