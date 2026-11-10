//! Private build-time layouts. No exported verbs symbol is linked here.
#![allow(
    non_camel_case_types,
    non_upper_case_globals,
    non_snake_case,
    dead_code
)]
#![allow(clippy::all, clippy::undocumented_unsafe_blocks)]
// SAFETY: bindgen derives C layouts and zero-initializing Default implementations
// from the build host's verbs.h. Only the local port shim has a linked declaration.
include!(concat!(env!("OUT_DIR"), "/verbs.rs"));

pub(in crate::transport::rdma) use super::native::{
    _ibv_query_gid_ex, ibv_ack_async_event, ibv_ack_cq_events, ibv_alloc_pd, ibv_close_device,
    ibv_create_comp_channel, ibv_create_cq, ibv_create_qp, ibv_dealloc_pd, ibv_dereg_mr,
    ibv_destroy_comp_channel, ibv_destroy_cq, ibv_destroy_qp, ibv_free_device_list,
    ibv_get_async_event, ibv_get_cq_event, ibv_get_device_list, ibv_get_device_name, ibv_modify_qp,
    ibv_open_device, ibv_query_device, ibv_query_gid, ibv_reg_mr,
};

#[cfg(test)]
pub(in crate::transport::rdma) use super::native::ibv_query_qp;
