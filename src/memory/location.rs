//! Owner-derived memory classification and bounded location metadata.

use std::fmt;

/// Host or CUDA device memory, determined by the allocation owner.
/// This enum is non-exhaustive; consumers must handle unknown variants.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum MemoryType {
    /// Host-accessible bytes, including CUDA-pinned host allocations.
    Cpu,
    /// CUDA device allocation; CPU pointer dereference is not authorized.
    Cuda,
}

/// An owner's location, not permission to access its bytes.
/// This enum is non-exhaustive; consumers must handle unknown variants.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
#[non_exhaustive]
pub enum MemoryLocation {
    /// Unknown placement is `None`, including ordinary and portable pinned DRAM.
    Cpu(Option<u32>),
    /// CUDA-visible ordinal and process-local native context identity. The
    /// numeric context does not retain or authorize use of a native handle.
    Cuda {
        /// CUDA-visible device ordinal.
        device: i32,
        /// Process-local context identity; not an owned native handle.
        context: usize,
    },
}

impl MemoryLocation {
    /// Return the backing class without querying a native driver.
    pub fn memory_type(self) -> MemoryType {
        match self {
            Self::Cpu(_) => MemoryType::Cpu,
            Self::Cuda { .. } => MemoryType::Cuda,
        }
    }
}

impl fmt::Display for MemoryLocation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cpu(Some(node)) => write!(f, "cpu:{node}"),
            Self::Cpu(None) => f.write_str("*"),
            Self::Cuda { device, .. } => write!(f, "cuda:{device}"),
        }
    }
}

/// A checked subrange of a live owner. This metadata does not hold a transfer
/// loan, wait for a producer, or extend the lifetime of the memory/context.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct RangeLocation {
    /// Base address of the checked range, valid only while its real owner lives.
    pub address: u64,
    /// Range length in bytes.
    pub length: usize,
    /// Owner-derived placement snapshot.
    pub location: MemoryLocation,
}
