//! Fallible native verbs contract; resources stay with the calling Rust owner.

#![allow(unsafe_code)]

use std::ffi::{c_char, c_int, c_void};

use orbit_rdma::provider;
pub(in crate::transport::rdma) use orbit_rdma::provider::abi as ffi;
pub(super) use provider::{
    AsyncEvent, ChannelHandles, ConnectionHandles, CqHandles, DeviceHandles, GPU_PROBE_QPN,
    PostResult, ReadDepth,
};

use super::{
    Descriptor, RdmaConfig,
    discovery::{self, RdmaAddress},
};
use crate::runtime::topology::Topology;
use crate::{Error, Result};

mod cq;
mod device;
#[cfg(feature = "cuda")]
mod gpu_registration;
mod memory;
#[cfg(feature = "cuda")]
pub(crate) use gpu_registration::GpuRegistrationVerbs;
#[cfg(all(test, feature = "cuda"))]
pub(super) use provider::mlx5::peermem_at;
mod qp;

use cq::poll;
pub(super) use device::devices;
use device::query_address;
#[cfg(all(test, feature = "cuda"))]
pub(super) use memory::register_on_pd;
use memory::{allocate_window, dealloc_window, prepare_grant, register_memory, window};
use provider::{
    ack_async_event, ack_completion_event, arm_completion, close_completion_channel, close_device,
    create_completion_channel, create_cq, dealloc_pd, destroy_cq, destroy_qp, get_async_event,
    get_completion_event, open_device,
};
use qp::{connect, open, post, post_receive, send_completion};

#[derive(Clone, Copy)]
pub(super) struct CqEventVerbs {
    pub create: unsafe extern "C" fn(*mut ChannelHandles, *const DeviceHandles) -> c_int,
    pub get: unsafe extern "C" fn(*mut c_void, *mut *mut c_void) -> c_int,
    pub ack: unsafe extern "C" fn(*mut c_void),
    pub arm: unsafe extern "C" fn(*mut c_void) -> c_int,
    pub close: unsafe extern "C" fn(*mut ChannelHandles) -> c_int,
}

#[derive(Clone, Copy)]
pub(super) struct GpuKeyVerbs {
    pub create: unsafe extern "C" fn(
        *const DeviceHandles,
        *mut MemoryHandles,
        u32,
        u64,
        u32,
        u32,
        u32,
        bool,
    ) -> c_int,
    pub destroy: unsafe extern "C" fn(*mut c_void) -> c_int,
}

#[derive(Default)]
#[repr(C)]
pub(crate) struct MemoryHandles {
    #[cfg(feature = "cuda")]
    pub(crate) dmabuf: Option<crate::cuda::rdma::DmaBuf>,
    #[cfg(feature = "cuda")]
    pub(crate) dmabuf_verb: Option<gpu_registration::DmaBufVerb>,
    pub(crate) gpu_route: u8,
    pub(super) deregister_failure: c_int,
    pub(super) native: provider::Memory,
    pub gpu_key_failure: c_int,
}

impl std::ops::Deref for MemoryHandles {
    type Target = provider::Memory;
    fn deref(&self) -> &Self::Target {
        &self.native
    }
}
impl std::ops::DerefMut for MemoryHandles {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.native
    }
}

impl MemoryHandles {
    pub(super) fn retire(&mut self, verbs: &Verbs) -> std::result::Result<(), (&'static str, i32)> {
        if self.deregister_failure == 0 {
            // SAFETY: the caller retains the MR and its backing through this receipt.
            self.deregister_failure = unsafe { (verbs.deregister)(self) };
        }
        if self.deregister_failure != 0 {
            return Err(("ibv_dereg_mr", self.deregister_failure));
        }
        #[cfg(feature = "cuda")]
        if let Some(fd) = &mut self.dmabuf {
            fd.close()?;
        }
        #[cfg(feature = "cuda")]
        {
            self.dmabuf = None;
        }
        Ok(())
    }
    pub(super) fn destroy_gpu_key(
        &mut self,
        verbs: &Verbs,
    ) -> std::result::Result<(), (&'static str, i32)> {
        if self.gpu_key.is_null() {
            return if self.gpu_key_failure == 0 {
                Ok(())
            } else {
                // CREATE returned no object and no definite rejection. There
                // is no handle that could give a destruction receipt.
                Err(("mlx5 DevX GPU key creation", self.gpu_key_failure))
            };
        }
        if self.gpu_key_failure == 0 {
            let table = verbs
                .gpu_keys
                .ok_or(("missing GPU key destroy", libc::ENOSYS))?;
            // SAFETY: the grant or unpublished probe owns the object and its parent MR.
            self.gpu_key_failure = unsafe { (table.destroy)(self.gpu_key) };
        }
        if self.gpu_key_failure != 0 {
            return Err(("mlx5dv_devx_obj_destroy GPU grant", self.gpu_key_failure));
        }
        self.gpu_key = std::ptr::null_mut();
        Ok(())
    }
}

#[derive(Default)]
#[repr(C)]
pub(super) struct Completion {
    pub id: u64,
    pub status: u32,
    pub opcode: u32,
    pub length: u32,
    pub vendor_error: u32,
    pub qpn: u32,
    pub flags: u32,
    pub invalidated_rkey: u32,
}

pub(super) const FLUSHED: u32 = ffi::IBV_WC_WR_FLUSH_ERR;

pub(super) const GRH: u32 = ffi::IBV_WC_GRH;
pub(super) const WITH_INVALIDATE: u32 = ffi::IBV_WC_WITH_INV;

#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub(super) struct WorkRequest {
    pub id: u64,
    pub offset: u32,
    pub length: u32,
    pub signaled: bool,
}

#[derive(Clone, Copy)]
pub(super) struct AsyncVerbs {
    pub get: unsafe extern "C" fn(*mut c_void, *mut AsyncEvent) -> c_int,
    pub ack: unsafe extern "C" fn(*mut c_void, *mut AsyncEvent),
}

type QueryPortSpeed = unsafe fn(*const DeviceHandles, u8) -> Result<Option<u64>>;
type Discover = fn(&RdmaConfig) -> Result<(Topology, Vec<RdmaAddress>)>;
type CloseConnection = unsafe extern "C" fn(*mut ConnectionHandles) -> c_int;
type CloseMemory = unsafe extern "C" fn(*mut MemoryHandles) -> c_int;
pub(crate) struct Verbs {
    #[cfg(feature = "cuda")]
    pub(crate) gpu_registration: GpuRegistrationVerbs,
    pub(in crate::transport::rdma) gpu_keys: Option<GpuKeyVerbs>,
    pub(in crate::transport::rdma) asynchronous: Option<AsyncVerbs>,
    pub(in crate::transport::rdma) completion: Option<CqEventVerbs>,
    pub(in crate::transport::rdma) discover: Discover,
    pub(in crate::transport::rdma) query_address:
        unsafe fn(*const DeviceHandles, &RdmaAddress, Option<u8>) -> Result<RdmaAddress>,
    pub(in crate::transport::rdma) query_port_speed: Option<QueryPortSpeed>,
    pub(in crate::transport::rdma) open_device:
        unsafe extern "C" fn(*mut DeviceHandles, *const c_char) -> c_int,
    pub(in crate::transport::rdma) create_cq:
        unsafe extern "C" fn(*mut CqHandles, *const DeviceHandles, u32, *mut c_void) -> c_int,
    pub(in crate::transport::rdma) register_memory: unsafe extern "C" fn(
        *mut MemoryHandles,
        *const DeviceHandles,
        *mut c_void,
        usize,
        bool,
    ) -> c_int,
    pub(in crate::transport::rdma) prepare_grant: unsafe extern "C" fn(
        *mut ConnectionHandles,
        *mut MemoryHandles,
        *const DeviceHandles,
        *mut c_void,
        usize,
        u32,
    ) -> c_int,
    pub(in crate::transport::rdma) open: unsafe extern "C" fn(
        *mut ConnectionHandles,
        *mut MemoryHandles,
        *const DeviceHandles,
        u8,
        u8,
        *const Descriptor,
        *mut c_void,
        usize,
        u32,
        u32,
        u32,
        u32,
        *mut Descriptor,
    ) -> c_int,
    pub(in crate::transport::rdma) connect: unsafe extern "C" fn(
        *mut ConnectionHandles,
        *const Descriptor,
        *const Descriptor,
        u8,
        u8,
        u8,
        u8,
        u8,
    ) -> c_int,
    pub(in crate::transport::rdma) post: unsafe extern "C" fn(
        *mut ConnectionHandles,
        *const MemoryHandles,
        *const WorkRequest,
        usize,
        u64,
        usize,
        u64,
        u32,
        c_int,
        *mut PostResult,
    ) -> c_int,
    pub(in crate::transport::rdma) window: unsafe extern "C" fn(
        *mut ConnectionHandles,
        *mut MemoryHandles,
        u64,
        u64,
        usize,
        u32,
        bool,
        *mut PostResult,
    ) -> c_int,
    pub(in crate::transport::rdma) post_receive: unsafe extern "C" fn(
        *mut ConnectionHandles,
        *const MemoryHandles,
        u64,
        u64,
        usize,
        *mut PostResult,
    ) -> c_int,
    pub(in crate::transport::rdma) send_completion: unsafe extern "C" fn(
        *mut ConnectionHandles,
        *const MemoryHandles,
        u64,
        u64,
        usize,
        u32,
        bool,
        *mut PostResult,
    ) -> c_int,
    pub(in crate::transport::rdma) allocate_window:
        unsafe extern "C" fn(*mut MemoryHandles, *const DeviceHandles) -> c_int,
    pub(in crate::transport::rdma) dealloc_window: CloseMemory,
    pub(in crate::transport::rdma) poll:
        unsafe extern "C" fn(*mut ConnectionHandles, *mut Completion) -> c_int,
    pub(in crate::transport::rdma) destroy_qp: CloseConnection,
    pub(in crate::transport::rdma) deregister: CloseMemory,
    pub(in crate::transport::rdma) destroy_cq: unsafe extern "C" fn(*mut CqHandles) -> c_int,
    pub(in crate::transport::rdma) dealloc_pd: unsafe extern "C" fn(*mut DeviceHandles) -> c_int,
    pub(in crate::transport::rdma) close_device: unsafe extern "C" fn(*mut DeviceHandles) -> c_int,
}

pub(super) fn install() -> Result<()> {
    provider::install().map_err(|e| Error::Unavailable(provider::load_diagnostic(&e)))
}

fn provider_error(error: orbit_rdma::Error) -> Error {
    match error {
        orbit_rdma::Error::Load(e) => Error::Unavailable(provider::load_diagnostic(&e)),
        orbit_rdma::Error::Native { operation, code } => Error::Backend { operation, code },
        orbit_rdma::Error::Invalid(reason) | orbit_rdma::Error::Unsupported(reason) => {
            Error::Unavailable(reason)
        }
        orbit_rdma::Error::DeviceList(e) => Error::Io(e),
        _ => Error::Internal("unexpected RDMA provider error"),
    }
}
unsafe fn query_port_speed(device: *const DeviceHandles, port: u8) -> Result<Option<u64>> {
    // SAFETY: the context owner holds the device during this synchronous port query.
    unsafe { provider::port::query_port_speed(device, port) }.map_err(provider_error)
}

pub(super) static REAL: Verbs = Verbs {
    #[cfg(feature = "cuda")]
    gpu_registration: gpu_registration::REAL,
    #[cfg(feature = "cuda")]
    gpu_keys: Some(GpuKeyVerbs {
        create: memory::create_gpu_key,
        destroy: provider::mlx5::destroy,
    }),
    #[cfg(not(feature = "cuda"))]
    gpu_keys: None,
    query_port_speed: Some(query_port_speed),
    discover: discovery::discover,
    query_address,
    completion: Some(CqEventVerbs {
        create: create_completion_channel,
        get: get_completion_event,
        ack: ack_completion_event,
        arm: arm_completion,
        close: close_completion_channel,
    }),
    asynchronous: Some(AsyncVerbs {
        get: get_async_event,
        ack: ack_async_event,
    }),
    open_device,
    create_cq,
    register_memory,
    prepare_grant,
    open,
    connect,
    post,
    window,
    post_receive,
    send_completion,
    allocate_window,
    dealloc_window,
    poll,
    destroy_qp,
    deregister: memory::deregister,
    destroy_cq,
    dealloc_pd,
    close_device,
};

#[cfg(test)]
#[path = "../../../tests/unit/transport/rdma/sys.rs"]
mod tests;
