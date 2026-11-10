//! Optional verbs entry point: no relocation against newer rdma-core.
use super::*;

pub(crate) type DmabufVerb =
    unsafe extern "C" fn(*mut c_void, u64, usize, u64, c_int, c_int) -> *mut c_void;

#[derive(Clone, Copy)]
pub(crate) struct GpuRegistrationVerbs {
    pub dmabuf: fn() -> Option<DmabufVerb>,
    pub peermem: fn() -> bool,
}

fn dmabuf() -> Option<DmabufVerb> {
    super::native::get().ok().and_then(|native| native.dmabuf)
}

pub(in crate::transport::rdma) fn peermem_at(root: &std::path::Path) -> bool {
    // NCCL's ibGdrSupportInitOnce recognizes both MLNX_OFED and NVIDIA modules.
    [
        "sys/kernel/mm/memory_peers/nv_mem/version",
        "sys/kernel/mm/memory_peers/nv_mem_nc/version",
        "sys/module/nvidia_peermem/version",
    ]
    .iter()
    .any(|path| root.join(path).exists())
}

pub(super) const REAL: GpuRegistrationVerbs = GpuRegistrationVerbs {
    dmabuf,
    peermem: || peermem_at(std::path::Path::new("/")),
};
