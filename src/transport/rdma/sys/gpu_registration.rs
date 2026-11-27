//! Optional verbs entry point: no relocation against newer rdma-core.
use super::*;

pub(crate) use provider::DmaBufVerb;

#[derive(Clone, Copy)]
pub(crate) struct GpuRegistrationVerbs {
    pub dmabuf: fn() -> Option<DmaBufVerb>,
    pub peermem: fn() -> bool,
}

pub(super) const REAL: GpuRegistrationVerbs = GpuRegistrationVerbs {
    dmabuf: provider::dmabuf_verb,
    peermem: || provider::mlx5::peermem_at(std::path::Path::new("/")),
};
