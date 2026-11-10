//! Process-lifetime, fallible libibverbs exports. Inline provider ops stay in their owners.
use std::ffi::{c_char, c_int, c_uint, c_void};
use std::sync::OnceLock;

use libloading::Library;

use super::ffi;
use crate::{Error, Result};

pub(super) type PortSpeed = unsafe extern "C" fn(*mut ffi::ibv_context, u32, *mut u64) -> c_int;
#[cfg(feature = "cuda")]
use super::gpu_registration::DmabufVerb;

macro_rules! exports {
    ($($name:ident($($arg:ident: $ty:ty),*) -> $ret:ty = $missing:expr;)+) => {
        pub(super) struct Native {
            $(pub(super) $name: unsafe extern "C" fn($($ty),*) -> $ret,)+
            pub(super) port_speed: Option<PortSpeed>,
            #[cfg(feature = "cuda")]
            pub(super) dmabuf: Option<DmabufVerb>,
            // OnceLock is never dropped. Every context/MR and callback retains this code.
            _library: Library,
        }
        impl Native {
            fn from_library(library: Library) -> std::result::Result<Self, &'static str> {
                Ok(Self {
                    $(
                        // SAFETY: signatures match the exported verbs.h declarations; this
                        // table retains the DSO for every pointer, including cleanup callbacks.
                        $name: *unsafe { library.get(concat!(stringify!($name), "\0").as_bytes()) }
                            .map_err(|_| concat!("libibverbs.so.1: missing ", stringify!($name)))?,
                    )+
                    // SAFETY: optional exports use their rdma-core C signatures and the same DSO.
                    port_speed: unsafe { library.get::<PortSpeed>(b"ibv_query_port_speed\0") }.ok().map(|p| *p),
                    #[cfg(feature = "cuda")]
                    // SAFETY: this is the exact optional dma-buf ABI, retained with the DSO.
                    dmabuf: unsafe { library.get::<DmabufVerb>(b"ibv_reg_dmabuf_mr\0") }.ok().map(|p| *p),
                    _library: library,
                })
            }
        }
        $(
            // Some exports are used as callbacks or only by installed native gates.
            #[allow(dead_code)]
            pub(in crate::transport::rdma) unsafe fn $name($($arg: $ty),*) -> $ret {
                match get() {
                    // SAFETY: the caller owns the native arguments; successful install pins
                    // all exports for the process lifetime, including retirement calls.
                    Ok(native) => unsafe { (native.$name)($($arg),*) },
                    Err(_) => {
                        // No native owner can exist after a failed install. Direct ABI probes
                        // still fail closed rather than calling a null entry or panicking.
                        // SAFETY: errno is this calling thread's writable libc slot.
                        unsafe { *libc::__errno_location() = libc::ENOSYS; }
                        $missing
                    }
                }
            }
        )+
        #[cfg(test)]
        const REQUIRED: &[&str] = &[$(stringify!($name)),+];
    };
}

exports! {
    ibv_get_device_list(count: *mut c_int) -> *mut *mut ffi::ibv_device = std::ptr::null_mut();
    ibv_free_device_list(list: *mut *mut ffi::ibv_device) -> () = ();
    ibv_get_device_name(device: *mut ffi::ibv_device) -> *const c_char = std::ptr::null();
    ibv_open_device(device: *mut ffi::ibv_device) -> *mut ffi::ibv_context = std::ptr::null_mut();
    ibv_close_device(context: *mut ffi::ibv_context) -> c_int = libc::ENOSYS;
    ibv_query_device(context: *mut ffi::ibv_context, attr: *mut ffi::ibv_device_attr) -> c_int = libc::ENOSYS;
    ibv_query_port(context: *mut ffi::ibv_context, port: u8, attr: *mut ffi::_compat_ibv_port_attr) -> c_int = libc::ENOSYS;
    ibv_query_gid(context: *mut ffi::ibv_context, port: u8, index: c_int, gid: *mut ffi::ibv_gid) -> c_int = libc::ENOSYS;
    _ibv_query_gid_ex(context: *mut ffi::ibv_context, port: u32, index: u32, entry: *mut ffi::ibv_gid_entry, flags: u32, size: usize) -> c_int = libc::ENOSYS;
    ibv_alloc_pd(context: *mut ffi::ibv_context) -> *mut ffi::ibv_pd = std::ptr::null_mut();
    ibv_dealloc_pd(pd: *mut ffi::ibv_pd) -> c_int = libc::ENOSYS;
    ibv_reg_mr(pd: *mut ffi::ibv_pd, address: *mut c_void, length: usize, access: c_int) -> *mut ffi::ibv_mr = std::ptr::null_mut();
    ibv_dereg_mr(mr: *mut ffi::ibv_mr) -> c_int = libc::ENOSYS;
    ibv_create_cq(context: *mut ffi::ibv_context, count: c_int, cq_context: *mut c_void, channel: *mut ffi::ibv_comp_channel, vector: c_int) -> *mut ffi::ibv_cq = std::ptr::null_mut();
    ibv_destroy_cq(cq: *mut ffi::ibv_cq) -> c_int = libc::ENOSYS;
    ibv_create_comp_channel(context: *mut ffi::ibv_context) -> *mut ffi::ibv_comp_channel = std::ptr::null_mut();
    ibv_destroy_comp_channel(channel: *mut ffi::ibv_comp_channel) -> c_int = libc::ENOSYS;
    ibv_get_cq_event(channel: *mut ffi::ibv_comp_channel, cq: *mut *mut ffi::ibv_cq, context: *mut *mut c_void) -> c_int = libc::ENOSYS;
    ibv_ack_cq_events(cq: *mut ffi::ibv_cq, count: c_uint) -> () = ();
    ibv_create_qp(pd: *mut ffi::ibv_pd, attr: *mut ffi::ibv_qp_init_attr) -> *mut ffi::ibv_qp = std::ptr::null_mut();
    ibv_modify_qp(qp: *mut ffi::ibv_qp, attr: *mut ffi::ibv_qp_attr, mask: c_int) -> c_int = libc::ENOSYS;
    ibv_query_qp(qp: *mut ffi::ibv_qp, attr: *mut ffi::ibv_qp_attr, mask: c_int, init: *mut ffi::ibv_qp_init_attr) -> c_int = libc::ENOSYS;
    ibv_destroy_qp(qp: *mut ffi::ibv_qp) -> c_int = libc::ENOSYS;
    ibv_get_async_event(context: *mut ffi::ibv_context, event: *mut ffi::ibv_async_event) -> c_int = libc::ENOSYS;
    ibv_ack_async_event(event: *mut ffi::ibv_async_event) -> () = ();
}

fn load(path: &std::ffi::OsStr) -> std::result::Result<Native, &'static str> {
    // SAFETY: load the requested provider DSO; all required exports are checked before use.
    let library = unsafe { Library::new(path) }
        .map_err(|_| "libibverbs.so.1: could not load the host rdma-core library")?;
    Native::from_library(library)
}

pub(super) fn get() -> Result<&'static Native> {
    static NATIVE: OnceLock<std::result::Result<Native, &'static str>> = OnceLock::new();
    NATIVE
        .get_or_init(|| load(std::ffi::OsStr::new("libibverbs.so.1")))
        .as_ref()
        .map_err(|reason| Error::Unavailable(reason))
}

#[cfg(test)]
#[path = "../../../../tests/unit/transport/rdma/sys/native.rs"]
mod tests;
