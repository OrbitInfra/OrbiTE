#![deny(missing_docs)]

use std::fmt;
use std::io;

/// A fallible OrbiTE operation. An error alone is not a physical retirement receipt.
pub type Result<T> = std::result::Result<T, Error>;

/// Configuration, admission, transfer or retirement failure.
///
/// Inspect [`crate::TransferFailure::remote_effect`] for a failed WRITE. An
/// unknown effect is never replayed. Timeout and cancellation do not release
/// backing; `Unquiesced` retains native owners and their charges.
/// This enum is non-exhaustive; consumers must handle unknown variants.
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// A typed configuration field violates its construction-time bounds.
    InvalidConfig(&'static str),
    /// The JSON loader rejected a path before starting any engine owner.
    Config {
        /// JSON field path, or the configuration document path.
        path: String,
        /// Explanation of the rejected value or input.
        reason: String,
    },
    /// JSON syntax/schema diagnostics, including the offending QoS field.
    InvalidQosConfig(String),
    /// A selector rule or QP pool is invalid before admission.
    InvalidSelectorConfig(String),
    /// A named or matched policy cannot select an eligible route.
    TransportPolicy {
        /// Policy responsible for the rejection.
        policy: crate::PolicyName,
        /// Missing capability, unsupported hint or exhausted candidate reason.
        reason: &'static str,
    },
    /// Batch resources are still owned by work that has not physically drained.
    BatchBusy,
    /// A batch is sealed for submission, freed, or no longer available.
    BatchClosed,
    /// Waiting for a stable terminal result requires sealing the batch first.
    BatchOpen,
    /// Requester admission has been closed.
    RequesterClosed,
    /// This requester's retained endpoint/source-epoch identity is fenced against new work.
    PeerClosed,
    /// A public task index is outside the submitted range.
    InvalidTask,
    /// Cancellation was observed; inspect the separate retirement receipt.
    Cancelled,
    /// Native work or teardown has no proven quiescence; owners stay charged.
    Unquiesced {
        /// Native operation whose completion or cleanup could not be proved.
        operation: &'static str,
        /// Native status or errno retained for diagnostics.
        code: i32,
    },
    /// A provider, native library, capability or requested operation is unavailable.
    Unavailable(&'static str),
    /// A checked native call failed; retirement is reported separately.
    Backend {
        /// Failing native operation.
        operation: &'static str,
        /// Native status or errno.
        code: i32,
    },
    /// A range is empty, overflows, or exceeds its allocation or registration.
    InvalidRange,
    /// The registration or file does not grant the requested access.
    PermissionDenied,
    /// No current registration contains the requested range or identifier.
    RegionNotFound,
    /// A registration or segment belongs to another engine.
    ForeignRegistration,
    /// A local file handle is closed or does not belong to this engine.
    FileSegmentClosed,
    /// A resolved QoS policy belongs to another engine.
    ForeignQosPolicy,
    /// Overlapping registrations do not share the required allocation owner.
    RegistrationConflict,
    /// Source admission has been closed.
    SourceClosed,
    /// A peer or credit generation differs from the authoritative expected epoch.
    EpochMismatch,
    /// A bounded owner, byte, descriptor or queue budget cannot admit more work.
    ResourceExhausted,
    /// A peer message, descriptor or serialized identity violates the protocol.
    Protocol(&'static str),
    /// A deadline elapsed; this does not prove physical termination.
    Timeout,
    /// Dispatch prediction exceeded the configured local deadline threshold.
    DeadlineInfeasible,
    /// An invariant, synchronization primitive or internal owner failed.
    Internal(&'static str),
    /// An operating-system I/O operation failed.
    Io(io::Error),
}

impl Error {
    // The task and its group report each retain the concrete error.
    pub(crate) fn for_report(&self) -> Self {
        match self {
            Self::BatchBusy => Self::BatchBusy,
            Self::BatchClosed => Self::BatchClosed,
            Self::BatchOpen => Self::BatchOpen,
            Self::RequesterClosed => Self::RequesterClosed,
            Self::PeerClosed => Self::PeerClosed,
            Self::InvalidTask => Self::InvalidTask,
            Self::Cancelled => Self::Cancelled,
            Self::InvalidRange => Self::InvalidRange,
            Self::PermissionDenied => Self::PermissionDenied,
            Self::RegionNotFound => Self::RegionNotFound,
            Self::ForeignRegistration => Self::ForeignRegistration,
            Self::FileSegmentClosed => Self::FileSegmentClosed,
            Self::ForeignQosPolicy => Self::ForeignQosPolicy,
            Self::RegistrationConflict => Self::RegistrationConflict,
            Self::SourceClosed => Self::SourceClosed,
            Self::EpochMismatch => Self::EpochMismatch,
            Self::ResourceExhausted => Self::ResourceExhausted,
            Self::Timeout => Self::Timeout,
            Self::DeadlineInfeasible => Self::DeadlineInfeasible,
            Self::InvalidConfig(msg) => Self::InvalidConfig(msg),
            Self::Config { path, reason } => Self::Config {
                path: path.clone(),
                reason: reason.clone(),
            },
            Self::Unavailable(msg) => Self::Unavailable(msg),
            Self::Protocol(msg) => Self::Protocol(msg),
            Self::Internal(msg) => Self::Internal(msg),
            Self::TransportPolicy { policy, reason } => Self::TransportPolicy {
                policy: policy.clone(),
                reason,
            },
            Self::InvalidSelectorConfig(msg) => Self::InvalidSelectorConfig(msg.clone()),
            Self::InvalidQosConfig(msg) => Self::InvalidQosConfig(msg.clone()),
            Self::Unquiesced { operation, code } => Self::Unquiesced {
                operation,
                code: *code,
            },
            Self::Backend { operation, code } => Self::Backend {
                operation,
                code: *code,
            },
            Self::Io(e) => Self::Io(match e.raw_os_error() {
                Some(n) => io::Error::from_raw_os_error(n),
                None => io::Error::new(e.kind(), e.to_string()),
            }),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidConfig(message) => write!(f, "invalid configuration: {message}"),
            Self::Config { path, reason } => {
                write!(f, "invalid configuration at {path}: {reason}")
            }
            Self::InvalidQosConfig(message) => write!(f, "invalid QoS configuration: {message}"),
            Self::TransportPolicy { policy, reason } => {
                write!(f, "transport policy {}: {reason}", policy.as_str())
            }
            Self::InvalidSelectorConfig(message) => {
                write!(f, "invalid selector configuration: {message}")
            }
            Self::BatchBusy => f.write_str("batch work is not physically drained"),
            Self::BatchClosed => f.write_str("batch is sealed or freed"),
            Self::BatchOpen => {
                f.write_str("seal the batch before waiting for a stable terminal result")
            }
            Self::RequesterClosed => f.write_str("requester admission is closed"),
            Self::PeerClosed => f.write_str("peer endpoint and source epoch are retired"),
            Self::InvalidTask => f.write_str("task is outside the submitted batch"),
            Self::Cancelled => f.write_str("transfer cancelled"),
            Self::Unquiesced { operation, code } => write!(
                f,
                "{operation} has unproven quiescence (status {code}); resources retained"
            ),
            Self::Unavailable(backend) => write!(f, "backend unavailable: {backend}"),
            Self::Backend { operation, code } => {
                write!(f, "{operation} failed with backend status {code}")
            }
            Self::InvalidRange => {
                f.write_str("transfer range is empty, overflowing or outside its registration")
            }
            Self::PermissionDenied => {
                f.write_str("registration does not permit the remote operation")
            }
            Self::RegionNotFound => f.write_str("registration is no longer exported"),
            Self::ForeignRegistration => {
                f.write_str("registration belongs to another engine or segment")
            }
            Self::FileSegmentClosed => f.write_str("file segment is not open on this engine"),
            Self::ForeignQosPolicy => f.write_str("QoS policy belongs to another engine"),
            Self::RegistrationConflict => {
                f.write_str("overlapping registrations must share the same HostMemory owner")
            }
            Self::SourceClosed => f.write_str("source admission is closed"),
            Self::EpochMismatch => {
                f.write_str("source epoch does not match authoritative ownership")
            }
            Self::ResourceExhausted => f.write_str("engine resource budget exhausted"),
            Self::Protocol(message) => write!(f, "invalid OrbiTE frame: {message}"),
            Self::DeadlineInfeasible => {
                f.write_str("transfer cannot meet its deadline at the selected transport rate")
            }
            Self::Timeout => f.write_str("operation deadline expired"),
            Self::Internal(message) => write!(f, "internal engine failure: {message}"),
            Self::Io(error) => write!(f, "transport I/O failed: {error}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for Error {
    fn from(error: io::Error) -> Self {
        match error.kind() {
            io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => Self::Timeout,
            _ => Self::Io(error),
        }
    }
}
