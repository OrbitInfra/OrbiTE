#![deny(missing_docs)]

use std::cmp::Reverse;
use std::fmt;
use std::fs::File;
use std::io::Read;
use std::net::SocketAddr;
use std::str::FromStr;

pub(crate) mod file;
pub(crate) use file::FileSegments;
pub use file::{FileSegment, FileSegmentInfo};

/// Target for both synchronous and asynchronous transfer submissions.
/// This enum is non-exhaustive; consumers must handle unknown variants.
#[derive(Clone, Copy)]
#[non_exhaustive]
pub enum TransferTarget<'a> {
    /// A peer descriptor snapshot with authoritative source epoch.
    Memory(&'a RemoteSegment),
    /// A local file handle; addresses are zero-based file byte offsets.
    File(&'a FileSegment),
}

impl<'a> From<&'a RemoteSegment> for TransferTarget<'a> {
    fn from(peer: &'a RemoteSegment) -> Self {
        Self::Memory(peer)
    }
}

impl<'a> From<&'a FileSegment> for TransferTarget<'a> {
    fn from(file: &'a FileSegment) -> Self {
        Self::File(file)
    }
}
mod metadata;
pub(crate) use metadata::ProcessIdentity;
pub(crate) use metadata::SegmentMetadata;

use crate::{Error, MemoryType, Permission, Result};

/// Random source-instance identity, not an authentication credential.
/// Display emits hexadecimal text; parsing malformed text returns [`Error::Protocol`].
/// Pin the value obtained from the authoritative owner for the peer lifetime.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SourceEpoch(pub(crate) [u8; 16]);

impl SourceEpoch {
    pub(crate) fn generate() -> Result<Self> {
        let mut bytes = [0; 16];
        File::open("/dev/urandom")?.read_exact(&mut bytes)?;
        if bytes == [0; 16] {
            return Err(Error::Internal(
                "reserved source epoch from entropy provider",
            ));
        }
        Ok(Self(bytes))
    }
}

impl fmt::Display for SourceEpoch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl FromStr for SourceEpoch {
    type Err = Error;

    fn from_str(input: &str) -> Result<Self> {
        if input.len() != 32 || !input.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(Error::Protocol("epoch must contain 32 hexadecimal digits"));
        }
        let mut bytes = [0; 16];
        for (index, byte) in bytes.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&input[index * 2..index * 2 + 2], 16)
                .map_err(|_| Error::Protocol("epoch must contain 32 hexadecimal digits"))?;
        }
        Ok(Self(bytes))
    }
}

/// Observed native CUDA registration routes; neither availability nor a drain receipt.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub struct GpuDirectRoutes {
    /// At least one native MR for this registration used an exported dma-buf.
    pub dmabuf: bool,
    /// At least one native MR for this registration used NVIDIA/MLNX_OFED peermem.
    pub peermem: bool,
}
impl GpuDirectRoutes {
    pub(crate) fn bits(self) -> u8 {
        u8::from(self.dmabuf) | (u8::from(self.peermem) << 1)
    }
    pub(crate) fn from_bits(bits: u8) -> Self {
        Self {
            dmabuf: bits & 1 != 0,
            peermem: bits & 2 != 0,
        }
    }
}

/// A source-owned registration ID and bounds, fenced to one source instance.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RemoteRegion {
    pub(crate) gpu_direct_routes: GpuDirectRoutes,
    pub(crate) epoch: SourceEpoch,
    pub(crate) id: u64,
    pub(crate) address: u64,
    pub(crate) length: usize,
    pub(crate) permission: Permission,
    pub(crate) memory_type: MemoryType,
    pub(crate) ipc_exportable: bool,
    pub(crate) hp_registered: bool,
    pub(crate) profile: RegionProfile,
    pub(crate) fabric: Option<crate::transport::mnnvl::Location>,
}

/// Optional per-registration route and location carried by Describe.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RegionProfile {
    pub route: crate::Transport,
    pub node: Option<u32>,
}
impl Default for RegionProfile {
    fn default() -> Self {
        Self {
            route: crate::Transport::Unspec,
            node: None,
        }
    }
}
impl RegionProfile {
    pub(crate) fn permits(self, t: crate::Transport) -> bool {
        self.route == crate::Transport::Unspec || self.route == t
    }
    pub(crate) const SIZE: usize = 5;
    pub(crate) fn encode(self, dst: &mut [u8]) {
        dst[0] = self.route.wire_id();
        dst[1..5].copy_from_slice(&self.node.unwrap_or(u32::MAX).to_be_bytes());
    }
    pub(crate) fn decode(src: &[u8]) -> Result<Self> {
        let [route, a, b, c, d] = *src else {
            return Err(Error::Protocol("invalid registration profile"));
        };
        let index = u32::from_be_bytes([a, b, c, d]);
        Ok(Self {
            route: crate::Transport::from_wire_id(route)
                .ok_or(Error::Protocol("unknown registration transport"))?,
            node: (index != u32::MAX).then_some(index),
        })
    }
}

impl RemoteRegion {
    /// Native registration evidence at Describe time, not a liveness or release
    /// receipt. Re-read the descriptor after additional registrations to refresh it.
    pub fn gpu_direct_routes(&self) -> GpuDirectRoutes {
        self.gpu_direct_routes
    }
    /// Buffer location as published by its allocation owner.
    pub fn location_name(&self) -> String {
        match (self.memory_type, self.profile.node) {
            (MemoryType::Cpu, None) => "*".into(),
            (MemoryType::Cpu, Some(n)) => format!("cpu:{n}"),
            (MemoryType::Cuda, Some(n)) => format!("cuda:{n}"),
            (MemoryType::Cuda, None) => "cuda".into(),
        }
    }
    /// Source-owned backing classification from the descriptor snapshot.
    pub fn memory_type(&self) -> MemoryType {
        self.memory_type
    }

    /// Source registration identity, meaningful only within its source epoch.
    pub fn id(&self) -> u64 {
        self.id
    }
    /// Registered remote extent in bytes.
    pub fn len(&self) -> usize {
        self.length
    }
    /// Whether the descriptor has no bytes.
    pub fn is_empty(&self) -> bool {
        self.length == 0
    }
    /// Access granted by the source at descriptor publication.
    pub fn permission(&self) -> Permission {
        self.permission
    }
    /// Remote virtual base address; never dereference it in the requester process.
    pub fn base_address(&self) -> u64 {
        self.address
    }

    pub(crate) fn offset_of(&self, address: u64, length: usize) -> Option<usize> {
        let offset = usize::try_from(address.checked_sub(self.address)?).ok()?;
        let end = offset.checked_add(length)?;
        (length != 0 && end <= self.length).then_some(offset)
    }
}

/// A requester-owned descriptor snapshot. Clones retain its admission identity.
/// Refresh explicitly with the same authoritative epoch. To use another engine,
/// call that engine's `open_segment`; a borrowed snapshot cannot create admission.
/// The handle retains this requester's identity/fence entry, not the peer's
/// allocation. Each admitted transfer acquires its own source authority.
/// [`crate::TransferEngine::close_segment`] fences admission and drains cached
/// owners; timeout or uncertain cleanup returns [`Error::Unquiesced`]. Dropping
/// a descriptor is not a drain receipt.
#[derive(Clone, Debug)]
pub struct RemoteSegment {
    pub(crate) identity: Option<crate::runtime::segment_manager::PeerHandle>,
    pub(crate) endpoint: SocketAddr,
    pub(crate) epoch: SourceEpoch,
    pub(crate) regions: Vec<RemoteRegion>,
    pub(crate) metadata: SegmentMetadata,
}

impl RemoteSegment {
    /// Control endpoint bound to this requester-owned snapshot.
    pub fn endpoint(&self) -> SocketAddr {
        self.endpoint
    }
    /// Source-instance epoch supplied by the authoritative consumer registry.
    pub fn source_epoch(&self) -> SourceEpoch {
        self.epoch
    }
    /// Registration descriptors captured at open time; refresh by reopening the same epoch.
    pub fn regions(&self) -> &[RemoteRegion] {
        &self.regions
    }

    /// Resolves a source-granted absolute address within this epoch's descriptor
    /// snapshot. Uses the first containing buffer in ascending address order,
    /// preferring the longer buffer at equal bases.
    /// This lookup does not acquire a source grant or refresh authority.
    ///
    /// # Errors
    /// Returns [`Error::InvalidRange`] for an empty or overflowing range and
    /// [`Error::RegionNotFound`] when no descriptor contains it.
    pub fn resolve_address(&self, address: u64, length: usize) -> Result<(RemoteRegion, usize)> {
        if length == 0 || address.checked_add(length as u64).is_none() {
            return Err(Error::InvalidRange);
        }
        self.regions
            .iter()
            .filter_map(|region| {
                region
                    .offset_of(address, length)
                    .map(|offset| (*region, offset))
            })
            .min_by_key(|(region, _)| (region.base_address(), Reverse(region.len())))
            .ok_or(Error::RegionNotFound)
    }
}

#[cfg(test)]
#[path = "../tests/unit/segment.rs"]
mod tests;
