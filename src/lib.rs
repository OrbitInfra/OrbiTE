//! Owned CPU/NVIDIA memory, fenced TCP/direct or staged RDMA transfers and notifications.
//!
//! Memory, registrations and staging stay retained until physical completion.
//! Rust is the primary API; Python mirrors OrbiTE's retained owners. Peers use OrbiTE's
//! native protocol and must run the same build. Hardware qualification is separate.
//! See the repository's completion plan for qualification and integration gates.
//!
//! ```
//! use std::time::Duration;
//! use orbite::{EngineConfig, Permission, TransferEngine, TransferOp, TransferRequest};
//!
//! # fn main() -> orbite::Result<()> {
//! let source = TransferEngine::new(EngineConfig::default())?;
//! let exported = source.register_memory(b"orbit".to_vec().into_boxed_slice(), Permission::RemoteRead)?;
//! let requester = TransferEngine::new(EngineConfig::default())?;
//! let destination = requester.register_memory(vec![0; 5].into_boxed_slice(), Permission::LocalOnly)?;
//! // Obtain this epoch from the authoritative source owner, not untrusted discovery.
//! let peer = requester.open_segment(source.endpoint(), source.source_epoch(), Duration::from_secs(1))?;
//! let remote = peer.regions().iter().find(|r| r.id() == exported.id()).ok_or(orbite::Error::RegionNotFound)?;
//! let request = TransferRequest::new(
//!     orbite::Transport::Tcp,
//!     destination.base_address(),
//!     remote.base_address(),
//!     5,
//! );
//! let report = requester.submit_and_wait(TransferOp::Read, &peer, &[request], Duration::from_secs(1))?;
//! assert!(report.is_complete());
//! let mut bytes = [0; 5];
//! destination.read(0, &mut bytes)?;
//! assert_eq!(&bytes, b"orbit");
//! source.close_source(Duration::from_secs(1))?;
//! # Ok(())
//! # }
//! ```

#![cfg_attr(docsrs, feature(doc_cfg))]
#![deny(unsafe_code)]
#![deny(unsafe_op_in_unsafe_fn)]

#[cfg(unsupported_native_target)]
compile_error!("OrbiTE's `cuda`, `rdma`, `io-uring`, and `gds` features require a Linux target");

pub mod diagnostics;

#[cfg(not(unsupported_native_target))]
mod batch;
#[cfg(not(unsupported_native_target))]
mod config;
#[cfg(not(unsupported_native_target))]
mod engine;
#[cfg(not(unsupported_native_target))]
mod error;
#[cfg(not(unsupported_native_target))]
mod memory;
#[cfg(not(unsupported_native_target))]
#[cfg(feature = "metrics")]
#[cfg_attr(docsrs, doc(cfg(feature = "metrics")))]
pub mod metrics;
#[cfg(not(unsupported_native_target))]
mod notification;
#[cfg(target_os = "linux")]
mod platform;
#[cfg(not(unsupported_native_target))]
mod runtime;
#[cfg(not(unsupported_native_target))]
mod segment;
/// Transport-specific configuration and observation types.
#[cfg(not(unsupported_native_target))]
pub mod transport;

#[cfg(all(target_os = "linux", feature = "gds"))]
#[cfg_attr(docsrs, doc(cfg(all(target_os = "linux", feature = "gds"))))]
pub use transport::gds::{GdsMode, GdsStats};
#[cfg(all(target_os = "linux", feature = "io-uring"))]
#[cfg_attr(docsrs, doc(cfg(all(target_os = "linux", feature = "io-uring"))))]
pub use transport::io_uring::{IoUringConfig, IoUringStats};
#[cfg(all(target_os = "linux", feature = "cuda"))]
#[cfg_attr(docsrs, doc(cfg(all(target_os = "linux", feature = "cuda"))))]
pub mod cuda;
#[cfg(all(target_os = "linux", not(unsupported_native_target)))]
pub use runtime::topology::{
    DEVICE_PRIORITY_RANKS, MemEntry, NicEntry, NicType, Topology, TopologyMemoryKind,
};
#[cfg(not(unsupported_native_target))]
pub use transport::{
    hp_tcp::HpTcpConfig,
    mnnvl::{MnnvlConfig, MnnvlHandleType},
    shm::ShmConfig,
};
#[cfg(all(target_os = "linux", feature = "cuda"))]
#[cfg_attr(docsrs, doc(cfg(all(target_os = "linux", feature = "cuda"))))]
pub use {
    config::StagingLocation, runtime::proxy_manager::StagingStats, transport::nvlink::NvlinkStats,
};

#[cfg(not(unsupported_native_target))]
pub use {
    batch::{Batch, BatchId, BatchState, BatchStatus, SubmissionStatus, TaskState, TaskStatus},
    config::{EngineConfig, RuntimeQueueConfig},
    engine::{
        BatchReport, EngineStats, FileIoElement, Priority, RemoteEffect, TransferEngine,
        TransferFailure, TransferOp, TransferRequest, Transport,
    },
    error::{Error, Result},
    memory::{
        HostMemory, MemoryBacking, MemoryLocation, MemoryType, Permission, RangeLocation,
        RegisteredMemory,
    },
    notification::{Notification, NotificationEffect, NotificationFailure, NotificationStatus},
    runtime::{
        admission_queue::RuntimeQueueStats,
        progress_worker::{next_poll_delay, wait_before_next_poll},
        qos_contract::{
            EffectiveQosPolicy, QosConfig, QosDegradedActions, QosPolicyFields, QosRequestContext,
            QosTenantContract, QosUsage,
        },
        receiver_credit::{
            CreditAmount, CreditBalance, CreditResource, CreditUpdateDisposition,
            ReceiverCreditConfig, ReceiverCreditIssuerStats, ReceiverCreditStats,
            ReceiverCreditUpdateV1,
        },
        transport_selector::{
            IntentType, PolicyName, QpPool, SegmentType, SelectionPolicy, TransportSelectorConfig,
        },
    },
    segment::{
        FileSegment, FileSegmentInfo, GpuDirectRoutes, RemoteRegion, RemoteSegment, SourceEpoch,
        TransferTarget,
    },
};

#[cfg(all(test, not(unsupported_native_target)))]
#[path = "../tests/common/ports.rs"]
mod test_ports;
#[cfg(all(test, not(unsupported_native_target)))]
#[path = "../tests/common/mod.rs"]
mod test_support;
