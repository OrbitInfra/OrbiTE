//! Optional verbs entry point: no relocation against newer rdma-core.
use super::*;
use std::sync::OnceLock;

pub(crate) type DmabufVerb =
    unsafe extern "C" fn(*mut c_void, u64, usize, u64, c_int, c_int) -> *mut c_void;

#[derive(Clone, Copy)]
pub(crate) struct GpuRegistrationVerbs {
    pub dmabuf: fn() -> Option<DmabufVerb>,
    pub peermem: fn() -> bool,
}

fn dmabuf() -> Option<DmabufVerb> {
    static SYMBOL: OnceLock<Option<DmabufVerb>> = OnceLock::new();
    *SYMBOL.get_or_init(|| {
        // SAFETY: libibverbs is already linked for the lifetime of this process.
        // Only the optional, exact rdma-core C ABI is converted to a function.
        let symbol = unsafe { libc::dlsym(libc::RTLD_DEFAULT, c"ibv_reg_dmabuf_mr".as_ptr()) };
        if symbol.is_null() {
            None
        } else {
            // SAFETY: the non-null symbol has the ibv_reg_dmabuf_mr signature above.
            Some(unsafe { std::mem::transmute::<*mut c_void, DmabufVerb>(symbol) })
        }
    })
}

pub(super) const REAL: GpuRegistrationVerbs = GpuRegistrationVerbs {
    dmabuf,
    peermem: || std::path::Path::new("/sys/module/nvidia_peermem").is_dir(),
};
