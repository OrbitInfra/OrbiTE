//! Optional verbs entry point: no relocation against newer rdma-core.
use super::*;

pub(crate) use provider::DmaBufVerb;

#[derive(Clone, Copy)]
pub(crate) struct GpuRegistrationVerbs {
    pub dmabuf: fn() -> Option<DmaBufVerb>,
    pub peermem: fn() -> bool,
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
    dmabuf: provider::dmabuf_verb,
    peermem: || peermem_at(std::path::Path::new("/")),
};
